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

#[cfg(test)]
#[path = "signal_registration_tests.rs"]
mod registration_tests;

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

const LINK_REQUIRED: &str = "Signal has no registered link for this computer. Open messaging setup and scan a new QR code from Signal on your phone, then complete Note to Self verification. Existing account data has been kept.";
const CHECK_UNAVAILABLE: &str = "Consilium could not check Signal's registered accounts. No account data was changed and no new link was started. Check signal-cli and try again.";
const CHECK_TIMED_OUT: &str = "Checking Signal's registered accounts timed out. No account data was changed and no new link was started. Close any other account setup and try again.";
const INVALID_ACCOUNTS: &str = "signal-cli returned an unreadable registered-account list. No account data was changed and no new link was started. Update signal-cli and try again.";
const MAX_ACCOUNT_OUTPUT: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registration {
    Registered,
    LinkRequired,
    Unavailable(&'static str),
}

/// Only a successful structured list can establish that a phone link is missing.
/// Warning/error text can mention an unregistered account and is never evidence of a link.
fn parse_registration(output: &[u8], account: &str) -> Registration {
    if output.len() > MAX_ACCOUNT_OUTPUT {
        return Registration::Unavailable(INVALID_ACCOUNTS);
    }
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_slice(output) else {
        return Registration::Unavailable(INVALID_ACCOUNTS);
    };
    let mut matched = false;
    for row in rows {
        let Some(aci) = row.get("aci").and_then(serde_json::Value::as_str) else {
            return Registration::Unavailable(INVALID_ACCOUNTS);
        };
        if uuid::Uuid::parse_str(aci).is_err() {
            return Registration::Unavailable(INVALID_ACCOUNTS);
        }
        match row.get("number") {
            Some(serde_json::Value::String(number)) if bridge::valid_account(number) => {
                matched |= number == account;
            }
            Some(serde_json::Value::Null) => {}
            _ => return Registration::Unavailable(INVALID_ACCOUNTS),
        }
    }
    if matched { Registration::Registered } else { Registration::LinkRequired }
}

fn registration_probe(mut command: tokio::process::Command, account: &str, limit: Duration) -> Registration {
    use tokio::io::AsyncReadExt;
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, including the batch launcher.
    #[cfg(unix)]
    command.process_group(0);
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return Registration::Unavailable(CHECK_UNAVAILABLE);
    };
    runtime.block_on(async {
        let Ok(mut child) = command.spawn() else {
            return Registration::Unavailable(CHECK_UNAVAILABLE);
        };
        let Some(stdout) = child.stdout.take() else {
            stop_registration_probe(&mut child).await;
            return Registration::Unavailable(CHECK_UNAVAILABLE);
        };
        let mut output = Vec::new();
        let completed = tokio::time::timeout(limit, async {
            let mut reader = stdout.take((MAX_ACCOUNT_OUTPUT + 1) as u64);
            tokio::try_join!(child.wait(), reader.read_to_end(&mut output))
        }).await;
        match completed {
            Ok(Ok((status, _))) if status.success() => parse_registration(&output, account),
            Ok(_) => {
                stop_registration_probe(&mut child).await;
                Registration::Unavailable(CHECK_UNAVAILABLE)
            }
            Err(_) => {
                stop_registration_probe(&mut child).await;
                Registration::Unavailable(CHECK_TIMED_OUT)
            }
        }
    })
}

/// The Windows batch launcher owns a Java child. Ending only cmd.exe leaves the
/// account lock held, so cancel precisely this owned process tree before reaping it.
async fn stop_registration_probe(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        #[cfg(windows)]
        {
            let mut killer = tokio::process::Command::new("taskkill");
            killer.args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                .creation_flags(0x0800_0000).kill_on_drop(true);
            if let Ok(mut task) = killer.spawn() {
                if tokio::time::timeout(Duration::from_secs(5), task.wait()).await.is_err() {
                    let _ = task.kill().await;
                }
            }
        }
        #[cfg(unix)]
        // This probe was placed in its own group above; no other app is selected.
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
    }
    let _ = child.kill().await;
}

fn registered_account(cli: &std::path::Path, account: &str) -> Registration {
    let mut command = tokio::process::Command::new(cli);
    command.args(["--output", "json", "listAccounts"]);
    registration_probe(command, account, Duration::from_secs(30))
}

fn start_if_registered(registration: Registration, start: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    match registration {
        Registration::Registered => start(),
        Registration::LinkRequired => Err(LINK_REQUIRED.into()),
        Registration::Unavailable(detail) => Err(detail.into()),
    }
}

fn link_if_needed(registration: Registration, link: impl FnOnce() -> Result<(), String>) -> Result<bool, String> {
    match registration {
        Registration::Registered => Ok(false),
        Registration::LinkRequired => link().map(|()| true),
        Registration::Unavailable(detail) => Err(detail.into()),
    }
}

fn background_failure(run: u64, detail: String) {
    set(run, |state| {
        if !matches!(state.phase.as_str(), "linking" | "starting" | "verifying") {
            state.phase = "failed".into();
            state.link_uri = None;
            state.code = None;
            state.detail = detail;
        }
    });
}

/// UI readiness is separate from the raw daemon probe used during setup/startup.
pub fn connector_status(cfg: &BridgeConfig, timeout: Duration) -> bridge::ConnectorStatus {
    setup_connector_status(cfg, bridge::probe_signal(cfg, timeout), &state())
}

fn setup_connector_status(cfg: &BridgeConfig, mut status: bridge::ConnectorStatus, setup: &SetupState) -> bridge::ConnectorStatus {
    if !cfg.enabled {
        return status;
    }
    if status.state == "ok" {
        if cfg.allowed_acis.is_empty() {
            status.state = "down";
            status.detail = "Signal's daemon is running, but phone verification is not complete. Open messaging setup and finish Note to Self verification.".into();
        }
    } else if setup.phase == "failed" && [LINK_REQUIRED, CHECK_UNAVAILABLE, CHECK_TIMED_OUT, INVALID_ACCOUNTS].contains(&setup.detail.as_str()) {
        status.state = "down";
        status.detail = setup.detail.clone();
    }
    status
}

/// Whether a signal-cli command line is one an older Consilium left running that now does harm:
/// a daemon for `account` that receives without being asked (it drops phone messages that arrive
/// while Consilium restarts), or a `listAccounts` check that never exited.
pub fn stale_signal_cli(args: &[String], account: &str, running_secs: u64) -> bool {
    let Some(main) = args.iter().position(|a| a == "org.asamk.signal.Main") else {
        return false;
    };
    let rest = &args[main + 1..];
    if rest.iter().any(|a| a == "listAccounts") {
        return running_secs > 120;
    }
    let ours = rest.windows(2).any(|w| w[0] == "-a" && w[1] == account);
    ours && rest.iter().any(|a| a == "daemon") && !rest.iter().any(|a| a == "manual")
}

/// Checks the executable name before considering signal-cli arguments. AI prompts and
/// shell scripts can contain those same arguments and must never select their processes.
pub fn stale_signal_cli_process(name: &str, args: &[String], account: &str, running_secs: u64) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "java" | "java.exe" | "javaw" | "javaw.exe")
        && stale_signal_cli(args, account, running_secs)
}

/// Stops the signal-cli processes `stale_signal_cli` picks out. Returns how many were stopped.
fn stop_stale_signal_cli(account: &str) -> usize {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let mut stopped = 0;
    for p in sys.processes().values() {
        let args: Vec<String> = p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        if stale_signal_cli_process(&p.name().to_string_lossy(), &args, account, p.run_time()) && p.kill() {
            stopped += 1;
        }
    }
    stopped
}

/// Whether a process is a signal-cli daemon for `account`, in any receive mode. The process is
/// picked by name first (java running signal-cli, or a native signal-cli build), never by text alone.
pub fn daemon_for(name: &str, args: &[String], account: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let rest: &[String] = if matches!(name.as_str(), "java" | "java.exe" | "javaw" | "javaw.exe") {
        match args.iter().position(|a| a == "org.asamk.signal.Main") {
            Some(i) => &args[i + 1..],
            None => return false,
        }
    } else if matches!(name.as_str(), "signal-cli" | "signal-cli.exe") {
        args.get(1..).unwrap_or(&[])
    } else {
        return false;
    };
    rest.windows(2).any(|w| w[0] == "-a" && w[1] == account) && rest.iter().any(|a| a == "daemon")
}

/// After a fresh link, stops any daemon for the account (it still has the old keys) and waits
/// for the endpoint port to be free, so ensure_daemon starts one with the new link.
fn replace_old_daemon(run: u64, cfg: &BridgeConfig) -> Result<(), String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let mut stopped = 0;
    for p in sys.processes().values() {
        let args: Vec<String> = p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        if daemon_for(&p.name().to_string_lossy(), &args, &cfg.account) && p.kill() {
            stopped += 1;
        }
    }
    if stopped == 0 {
        return Ok(());
    }
    set(run, |s| {
        s.detail = format!("Stopped {stopped} older Signal background process(es) that still had the old link.");
    });
    let addr = endpoint_addr(cfg)?;
    let started = Instant::now();
    while TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
        if started.elapsed() > Duration::from_secs(15) {
            return Err(format!("An older signal-cli is still using {addr} after being stopped. Close it and click Try again."));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
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
    let registration = if daemon_answers(&cfg) { Registration::Registered } else { registered_account(&cli, &cfg.account) };
    let new_link = match link_if_needed(registration, || link(run, &cli, &cfg.account)) {
        Ok(new_link) => new_link,
        Err(detail) => return fail(run, detail),
    };
    if new_link {
        // A daemon started before this link still holds the old keys and the port, and
        // ensure_daemon would take it as running; replace it so the code reply reaches setup.
        if let Err(e) = replace_old_daemon(run, &cfg) {
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
            // Without the sender's account ID, the chat bridge would ignore every reply, so
            // "verified" would be a lie.
            let Some(a) = aci else {
                return fail(
                    run,
                    "Your reply arrived, but signal-cli didn't say which account sent it, so Consilium can't \
recognise your messages. Update signal-cli, then click Try again.",
                );
            };
            let mut next = BridgeConfig::load(&cfg_path).unwrap_or(cfg);
            next.enabled = true;
            if !next.allowed_acis.iter().any(|x| x.eq_ignore_ascii_case(&a)) {
                next.allowed_acis.push(a);
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
        if matches!(state().phase.as_str(), "linking" | "starting" | "verifying") {
            return;
        }
        let run = *RUN.lock().unwrap();
        let outcome = signal_cli().and_then(|cli| {
            // A daemon that already answers proves the account is linked; asking signal-cli
            // again would wait on the daemon's lock.
            if daemon_answers(&cfg) {
                return Ok(());
            }
            start_if_registered(registered_account(&cli, &cfg.account), || ensure_daemon(&cli, &cfg))
        });
        if let Err(detail) = outcome {
            background_failure(run, detail);
        }
    });
}

fn daemon_answers(cfg: &BridgeConfig) -> bool {
    let mut probe = cfg.clone();
    probe.enabled = true;
    bridge::probe_signal(&probe, Duration::from_secs(2)).state == "ok"
}

/// Called when the chat bridge can't reach the daemon: starts it again, at most once a minute.
pub fn revive(cfg: &BridgeConfig) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        *last = Some(Instant::now());
    }
    if matches!(state().phase.as_str(), "linking" | "starting" | "verifying") {
        return; // a setup in progress manages the daemon itself
    }
    let run = *RUN.lock().unwrap();
    let outcome = signal_cli().and_then(|cli| {
        if daemon_answers(cfg) {
            return Ok(());
        }
        start_if_registered(registered_account(&cli, &cfg.account), || ensure_daemon(&cli, cfg))
    });
    if let Err(detail) = outcome {
        background_failure(run, detail);
    }
}

fn endpoint_addr(cfg: &BridgeConfig) -> Result<SocketAddr, String> {
    bridge::validate_loopback(&cfg.endpoint)?;
    let raw = cfg.endpoint.trim().trim_start_matches("tcp://").trim_start_matches("http://");
    raw.parse().map_err(|_| "Automatic start needs an address like 127.0.0.1:7583 (Advanced)".to_string())
}

/// Starts `signal-cli daemon` in the background unless one already answers.
fn ensure_daemon(cli: &std::path::Path, cfg: &BridgeConfig) -> Result<(), String> {
    // Launch, setup and the chat bridge's restart can all get here at once; only one may start it.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let addr = endpoint_addr(cfg)?;
    let mut probe = cfg.clone();
    probe.enabled = true;
    if stop_stale_signal_cli(&cfg.account) > 0 {
        // Give the old daemon a moment to let go of the port and the account.
        std::thread::sleep(Duration::from_secs(3));
    }
    if bridge::probe_signal(&probe, Duration::from_secs(2)).state == "ok" {
        return Ok(());
    }
    // A daemon that is still catching up on messages can be too busy to answer the probe in time
    // but already holds the port and the account. Starting a second one only leaves a copy waiting
    // forever for the account lock, so a listening port counts as running.
    if TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok() {
        return Ok(());
    }
    let log = std::fs::File::create(std::env::temp_dir().join("consilium-signal-daemon.log")).ok();
    let mut cmd = Command::new(cli);
    // Manual receive mode: signal-cli only takes messages off Signal's servers while Consilium is
    // connected and subscribed, so anything sent while Consilium is closed or restarting waits on
    // the server instead of being received and dropped.
    cmd.args(["-a", &cfg.account, "daemon", "--tcp", &addr.to_string(), "--receive-mode", "manual"]).stdin(Stdio::null());
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
    w.write_all(crate::signal_chat::SUBSCRIBE.as_bytes()).map_err(|e| e.to_string())?;
    w.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(s);
    let started = Instant::now();
    let mut buf = Vec::new();
    while started.elapsed() < VERIFY_WINDOW {
        if *RUN.lock().unwrap() != run {
            return Err("Setup was restarted".into());
        }
        let Some(line) = crate::signal_chat::read_whole_line(&mut reader, &mut buf)? else { continue };
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
    let env = crate::signal_chat::receive_envelope(v)?;
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
    #[test]
    fn only_old_daemons_and_stuck_checks_are_stopped() {
        let a = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let acct = "+15551234567";
        let old = a("java -cp x org.asamk.signal.Main -a +15551234567 daemon --tcp 127.0.0.1:7583");
        let new = a("java -cp x org.asamk.signal.Main -a +15551234567 daemon --tcp 127.0.0.1:7583 --receive-mode manual");
        let other = a("java -cp x org.asamk.signal.Main -a +15559999999 daemon --tcp 127.0.0.1:7583");
        let list = a("java -cp x org.asamk.signal.Main listAccounts");
        assert!(stale_signal_cli(&old, acct, 5));
        assert!(!stale_signal_cli(&new, acct, 5));
        assert!(!stale_signal_cli(&other, acct, 5));
        assert!(!stale_signal_cli(&list, acct, 30));
        assert!(stale_signal_cli(&list, acct, 600));
        assert!(!stale_signal_cli(&a("java -jar something-else.jar daemon"), acct, 600));
    }

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

    #[test]
    fn the_reply_is_read_in_the_shape_signal_cli_sends_to_a_subscriber() {
        // After `subscribeReceive`, signal-cli wraps each message as params.result.envelope.
        let acc = "+15551234567";
        let aci = "8f0c6e1a-1111-4222-8333-944455556666";
        let sub = |env: serde_json::Value| serde_json::json!({"jsonrpc":"2.0","method":"receive",
            "params":{"subscription":0,"result":{"account":acc,"envelope":env}}});
        let mine = sub(serde_json::json!({"sourceNumber": acc, "sourceUuid": aci,
            "syncMessage": {"sentMessage": {"message": "8575", "destinationNumber": acc}}}));
        assert_eq!(reply_with_code(&mine, acc, "8575"), Some(Some(aci.to_string())));
        assert_eq!(reply_with_code(&mine, acc, "1234"), None);
        let stranger = sub(serde_json::json!({"sourceNumber": "+15559999999", "dataMessage": {"message": "8575"}}));
        assert_eq!(reply_with_code(&stranger, acc, "8575"), None);
    }
}

#[cfg(test)]
mod stale_cleanup_process_tests {
    use super::*;

    fn args(command: &str) -> Vec<String> {
        ["java", "org.asamk.signal.Main", "-a", "+15550001000", command]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn stale_cleanup_never_selects_ai_or_shell_processes() {
        for command in ["daemon", "listAccounts"] {
            let args = args(command);
            assert!(stale_signal_cli(&args, "+15550001000", 180));
            for name in ["claude.exe", "codex.exe", "node.exe", "powershell.exe", "pwsh.exe", "cmd.exe"] {
                assert!(
                    !stale_signal_cli_process(name, &args, "+15550001000", 180),
                    "selected {name} for {command}"
                );
            }
        }
    }

    #[test]
    fn stale_cleanup_still_selects_the_matching_java_process() {
        for command in ["daemon", "listAccounts"] {
            let args = args(command);
            for name in ["java", "java.exe", "javaw", "javaw.exe", "JAVA.EXE"] {
                assert!(stale_signal_cli_process(name, &args, "+15550001000", 180));
            }
        }
    }
}

#[cfg(test)]
mod relink_tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_manual_mode_daemon_for_the_account_is_replaced_after_relinking() {
        let args = v(&["java", "-cp", "x", "org.asamk.signal.Main", "-a", "+15550001111", "daemon", "--tcp", "127.0.0.1:7583", "--receive-mode", "manual"]);
        assert!(daemon_for("java.exe", &args, "+15550001111"));
        assert!(daemon_for("signal-cli", &v(&["signal-cli", "-a", "+15550001111", "daemon"]), "+15550001111"));
    }

    #[test]
    fn other_accounts_commands_and_processes_are_left_alone() {
        let other = v(&["java", "org.asamk.signal.Main", "-a", "+15550002222", "daemon"]);
        assert!(!daemon_for("java.exe", &other, "+15550001111"));
        let link = v(&["java", "org.asamk.signal.Main", "-a", "+15550001111", "link"]);
        assert!(!daemon_for("java.exe", &link, "+15550001111"));
        // An AI CLI or shell whose prompt mentions the same words is never picked.
        let shell = v(&["pwsh", "-c", "org.asamk.signal.Main -a +15550001111 daemon"]);
        assert!(!daemon_for("pwsh.exe", &shell, "+15550001111"));
        assert!(!daemon_for("claude.exe", &shell, "+15550001111"));
    }
}
