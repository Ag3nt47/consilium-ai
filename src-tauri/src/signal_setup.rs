//! Guided Signal setup that runs inside Consilium instead of a terminal window:
//! link this computer (QR code shown in the app), start signal-cli's daemon in the background,
//! then prove the round trip by sending a code to the user's Note to Self and waiting for it
//! to come back from their phone.
//!
//! Only fixed arguments reach signal-cli: a phone number that passed `bridge::valid_account`,
//! a loopback address that passed `bridge::validate_loopback`, and a device name built from
//! this computer's sanitised host name.

use crate::bridge::{self, BridgeConfig};
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What the setup panel shows. Never contains the full phone number.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct SetupState {
    /// "idle", "linking" (QR shown), "starting", "verifying", "verified" or "failed".
    pub phase: String,
    /// The `sgnl://linkdevice?…` address to draw as a QR code while `phase == "linking"`.
    pub link_uri: Option<String>,
    /// The code the user sends back from their phone while `phase == "verifying"`.
    pub code: Option<String>,
    pub detail: String,
    /// The name this computer appears under in Signal › Linked devices.
    pub device_name: String,
}

static STATE: Mutex<Option<SetupState>> = Mutex::new(None);
static LINK_CHILD: Mutex<Option<Child>> = Mutex::new(None);
/// Bumped whenever a new setup run starts, so a superseded run's thread stops writing state.
static RUN: Mutex<u64> = Mutex::new(0);

const VERIFY_WINDOW: Duration = Duration::from_secs(10 * 60);
const LINK_WINDOW: Duration = Duration::from_secs(5 * 60);

pub fn state() -> SetupState {
    STATE.lock().unwrap().clone().unwrap_or_else(|| SetupState {
        phase: "idle".into(),
        device_name: device_name(),
        ..Default::default()
    })
}

fn set(run: u64, f: impl FnOnce(&mut SetupState)) {
    if *RUN.lock().unwrap() != run {
        return;
    }
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(|| SetupState { device_name: device_name(), ..Default::default() });
    f(s);
}

fn fail(run: u64, why: impl Into<String>) {
    let why = why.into();
    set(run, |s| {
        s.phase = "failed".into();
        s.link_uri = None;
        s.code = None;
        s.detail = why;
    });
}

/// "Consilium (Lukes-MacBook)": unique per computer, so each one is recognisable in the
/// phone's Linked devices list. Only letters, digits, space, '-' and '.' survive.
pub fn device_name() -> String {
    let raw = sysinfo::System::host_name().unwrap_or_default();
    let raw = raw.trim_end_matches(".local");
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || " -.".contains(*c))
        .take(40)
        .collect();
    let fallback = if cfg!(target_os = "macos") {
        "Mac"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Linux"
    };
    format!("Consilium ({})", if clean.trim().is_empty() { fallback } else { clean.trim() })
}

fn signal_cli() -> Result<std::path::PathBuf, String> {
    bridge::find_signal_cli().ok_or_else(|| "Install signal-cli first (step 1)".to_string())
}

fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Whether signal-cli already holds keys for this number on this computer.
fn already_linked(cli: &std::path::Path, account: &str) -> bool {
    let out = quiet(Command::new(cli).arg("listAccounts")).stdin(Stdio::null()).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).lines().any(|l| l.split_whitespace().any(|w| w == account)),
        Err(_) => false,
    }
}

/// Pulls "+15551234567" out of signal-cli's "Associated with: +15551234567" line.
pub fn associated_number(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.split_once("Associated with:"))
        .map(|(_, rest)| rest.split_whitespace().next().unwrap_or("").to_string())
        .find(|n| bridge::valid_account(n))
}

/// Starts (or restarts) the whole guided setup. Returns at once; the panel polls `state()`.
pub fn begin(cfg_path: std::path::PathBuf) -> Result<SetupState, String> {
    let cfg = BridgeConfig::load(&cfg_path)?;
    if !bridge::valid_account(&cfg.account) {
        return Err("Save your Signal phone number first (step 2)".into());
    }
    let cli = signal_cli()?;
    cancel();
    let run = {
        let mut r = RUN.lock().unwrap();
        *r += 1;
        *r
    };
    *STATE.lock().unwrap() = Some(SetupState {
        phase: "linking".into(),
        detail: "Getting a QR code from signal-cli…".into(),
        device_name: device_name(),
        ..Default::default()
    });
    std::thread::spawn(move || run_setup(run, cli, cfg_path, cfg));
    Ok(state())
}

/// Stops a link in progress. A finished link and a running daemon are left alone.
pub fn cancel() {
    *RUN.lock().unwrap() += 1;
    if let Some(mut c) = LINK_CHILD.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    *STATE.lock().unwrap() = None;
}

fn run_setup(run: u64, cli: std::path::PathBuf, cfg_path: std::path::PathBuf, cfg: BridgeConfig) {
    if !already_linked(&cli, &cfg.account) {
        if let Err(e) = link(run, &cli, &cfg.account) {
            return fail(run, e);
        }
    }
    set(run, |s| {
        s.phase = "starting".into();
        s.link_uri = None;
        s.detail = "Linked. Starting Signal for Consilium in the background…".into();
    });
    if let Err(e) = ensure_daemon(&cli, &cfg) {
        return fail(run, e);
    }
    let code = format!("{:04}", rand_u32() % 10_000);
    set(run, |s| {
        s.phase = "verifying".into();
        s.code = Some(code.clone());
        s.detail = format!(
            "Sent a test message to your Signal \"Note to Self\" chat. Reply there with {code} to confirm it works."
        );
    });
    match verify(&cfg, &code, run) {
        Ok(aci) => {
            // Turn Signal on and allow the confirmed sender, keeping everything else as it was.
            let mut next = BridgeConfig::load(&cfg_path).unwrap_or(cfg);
            next.enabled = true;
            if let Some(a) = aci {
                if !next.allowed_acis.iter().any(|x| x.eq_ignore_ascii_case(&a)) {
                    next.allowed_acis.push(a);
                }
            }
            if let Err(e) = next.save(&cfg_path) {
                return fail(run, format!("Verified, but saving settings failed: {e}"));
            }
            set(run, |s| {
                s.phase = "verified".into();
                s.code = None;
                s.detail = "Verified: your phone and this computer can message each other. Signal is switched on.".into();
            });
        }
        Err(e) => fail(run, e),
    }
}

fn link(run: u64, cli: &std::path::Path, account: &str) -> Result<(), String> {
    let mut child = quiet(Command::new(cli).args(["link", "-n", &device_name()]))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start signal-cli: {e}"))?;
    let stdout = child.stdout.take().ok_or("signal-cli gave no output")?;
    let stderr = child.stderr.take();
    *LINK_CHILD.lock().unwrap() = Some(child);
    // Give up on an unscanned code after a while, so signal-cli isn't left waiting forever.
    std::thread::spawn(move || {
        std::thread::sleep(LINK_WINDOW);
        if *RUN.lock().unwrap() == run {
            if let Some(mut c) = LINK_CHILD.lock().unwrap().take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    });

    // signal-cli prints the sgnl:// address first, then "Associated with: …" once the phone
    // scans it. Collect both streams; the number line may land on either.
    let err_text = std::sync::Arc::new(Mutex::new(String::new()));
    if let Some(e) = stderr {
        let sink = err_text.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(e).lines().map_while(Result::ok).take(200) {
                let mut s = sink.lock().unwrap();
                s.push_str(&l);
                s.push('\n');
            }
        });
    }
    let mut out_text = String::new();
    for line in BufReader::new(stdout).lines().map_while(Result::ok).take(200) {
        let t = line.trim();
        if t.starts_with("sgnl://") && t.len() < 2048 {
            let uri = t.to_string();
            set(run, |s| {
                s.link_uri = Some(uri);
                s.detail = "On your phone: Signal › Settings › Linked devices › Link new device, then scan this code.".into();
            });
        } else {
            out_text.push_str(t);
            out_text.push('\n');
        }
    }
    let status = {
        let started = Instant::now();
        loop {
            let mut g = LINK_CHILD.lock().unwrap();
            let Some(c) = g.as_mut() else {
                return Err("The QR code expired after 5 minutes without being scanned. Click Try again.".into());
            };
            match c.try_wait() {
                Ok(Some(st)) => {
                    g.take();
                    break st;
                }
                Ok(None) if started.elapsed() > Duration::from_secs(10) => {
                    let _ = c.kill();
                    let _ = c.wait();
                    g.take();
                    return Err("signal-cli didn't finish linking".into());
                }
                Ok(None) => {}
                Err(e) => return Err(e.to_string()),
            }
            drop(g);
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    std::thread::sleep(Duration::from_millis(200));
    let all = format!("{out_text}{}", err_text.lock().unwrap());
    if !status.success() {
        let why = all.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no reason given");
        return Err(format!("Linking didn't finish: {}", crate::redact::scrub(why)));
    }
    match associated_number(&all) {
        Some(n) if n != account => Err(format!(
            "That phone's Signal account ends in {}, but the saved number ends in {}. Fix the number in step 2 and try again.",
            &n[n.len() - 4..],
            &account[account.len() - 4..]
        )),
        _ => Ok(()),
    }
}

/// On launch, starts signal-cli's daemon in the background if Signal is set up and switched on,
/// so it keeps working after a restart without a terminal window.
pub fn autostart(cfg_path: std::path::PathBuf) {
    std::thread::spawn(move || {
        let Ok(cfg) = BridgeConfig::load(&cfg_path) else { return };
        if !cfg.enabled || !bridge::valid_account(&cfg.account) {
            return;
        }
        if let Ok(cli) = signal_cli() {
            if already_linked(&cli, &cfg.account) {
                let _ = ensure_daemon(&cli, &cfg);
            }
        }
    });
}

fn endpoint_addr(cfg: &BridgeConfig) -> Result<SocketAddr, String> {
    bridge::validate_loopback(&cfg.endpoint)?;
    let raw = cfg.endpoint.trim().trim_start_matches("tcp://").trim_start_matches("http://");
    raw.parse().map_err(|_| "Automatic start needs an address like 127.0.0.1:7583 (Advanced)".to_string())
}

/// Starts `signal-cli daemon` in the background unless one already answers.
fn ensure_daemon(cli: &std::path::Path, cfg: &BridgeConfig) -> Result<(), String> {
    let addr = endpoint_addr(cfg)?;
    let mut probe = cfg.clone();
    probe.enabled = true;
    if bridge::probe_signal(&probe, Duration::from_secs(2)).state == "ok" {
        return Ok(());
    }
    let log = std::fs::File::create(std::env::temp_dir().join("consilium-signal-daemon.log")).ok();
    let mut cmd = Command::new(cli);
    cmd.args(["-a", &cfg.account, "daemon", "--tcp", &addr.to_string()]).stdin(Stdio::null());
    match log.and_then(|f| f.try_clone().ok().map(|g| (f, g))) {
        Some((o, e)) => cmd.stdout(o).stderr(e),
        None => cmd.stdout(Stdio::null()).stderr(Stdio::null()),
    };
    quiet(&mut cmd).spawn().map_err(|e| format!("couldn't start signal-cli: {e}"))?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_secs(1));
        if bridge::probe_signal(&probe, Duration::from_secs(2)).state == "ok" {
            return Ok(());
        }
    }
    Err("signal-cli started but isn't answering. Its log is consilium-signal-daemon.log in the temp folder.".into())
}

/// Sends the code to Note to Self, then waits for a message from the user's own account
/// containing it. Returns the sender's Signal account ID (ACI) when the daemon reports one.
fn verify(cfg: &BridgeConfig, code: &str, run: u64) -> Result<Option<String>, String> {
    let addr = endpoint_addr(cfg)?;
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?;
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    let msg = format!(
        "Consilium ({}) is linked. Reply to this Note to Self chat with {code} to confirm it works.",
        device_name()
    );
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": "consilium-verify", "method": "send",
        "params": { "noteToSelf": true, "message": msg }
    });
    w.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(s);
    let started = Instant::now();
    let mut line = String::new();
    while started.elapsed() < VERIFY_WINDOW {
        if *RUN.lock().unwrap() != run {
            return Err("Setup was restarted".into());
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Err("signal-cli closed the connection".into()),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e.to_string()),
        }
        if line.len() > 256 * 1024 {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v.get("id").and_then(|i| i.as_str()) == Some("consilium-verify") {
            if let Some(e) = v.get("error") {
                let m = e.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error");
                return Err(format!("Couldn't send the test message: {}", crate::redact::scrub(m)));
            }
            continue;
        }
        if let Some(aci) = reply_with_code(&v, &cfg.account, code) {
            return Ok(aci);
        }
    }
    Err("No reply with the code arrived within 10 minutes. Click Try again.".into())
}

/// A `receive` notification counts if it's from the user's own account (their phone typing in
/// Note to Self arrives as a sync "sent" message) and its text contains the code.
/// Returns Some(aci) on a match; the inner value is None when the daemon gave no account ID.
pub fn reply_with_code(v: &serde_json::Value, account: &str, code: &str) -> Option<Option<String>> {
    if v.get("method").and_then(|m| m.as_str()) != Some("receive") {
        return None;
    }
    let env = v.get("params")?.get("envelope")?;
    let from_self = env.get("sourceNumber").and_then(|n| n.as_str()) == Some(account)
        || env.get("source").and_then(|n| n.as_str()) == Some(account);
    if !from_self {
        return None;
    }
    let text = env
        .pointer("/syncMessage/sentMessage/message")
        .or_else(|| env.pointer("/dataMessage/message"))
        .and_then(|m| m.as_str())?;
    if !text.split(|c: char| !c.is_ascii_digit()).any(|w| w == code) {
        return None;
    }
    let aci = env
        .get("sourceUuid")
        .or_else(|| env.get("sourceAci"))
        .and_then(|u| u.as_str())
        .and_then(|u| uuid::Uuid::parse_str(u).ok())
        .map(|u| u.to_string());
    Some(aci)
}

fn rand_u32() -> u32 {
    // uuid v4 comes from the OS random source.
    let b = uuid::Uuid::new_v4().into_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_linked_number() {
        assert_eq!(associated_number("INFO x\nAssociated with: +15551234567\n").as_deref(), Some("+15551234567"));
        assert_eq!(associated_number("Associated with: nobody"), None);
    }

    #[test]
    fn device_name_is_safe_for_the_command_line() {
        let n = device_name();
        assert!(n.starts_with("Consilium ("));
        assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || " -.()".contains(c)));
    }

    #[test]
    fn only_the_users_own_reply_with_the_code_counts() {
        let acc = "+15551234567";
        let aci = "8f0c6e1a-1111-4222-8333-944455556666";
        let sync = serde_json::json!({"method":"receive","params":{"envelope":{
            "sourceNumber": acc, "sourceUuid": aci,
            "syncMessage": {"sentMessage": {"message": "ok 4821"}}}}});
        assert_eq!(reply_with_code(&sync, acc, "4821"), Some(Some(aci.to_string())));
        assert_eq!(reply_with_code(&sync, acc, "1234"), None);
        assert_eq!(reply_with_code(&sync, acc, "482"), None, "a partial code doesn't count");
        let stranger = serde_json::json!({"method":"receive","params":{"envelope":{
            "sourceNumber": "+15559999999", "dataMessage": {"message": "4821"}}}});
        assert_eq!(reply_with_code(&stranger, acc, "4821"), None);
    }
}
