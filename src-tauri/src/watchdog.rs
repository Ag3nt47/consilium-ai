use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const WATCHDOG_PORT: u16 = 8471;
pub const RELAUNCH_PROTOCOL: &str = "consilium-relaunch-v1";

/// Checks if an incoming network address is allowed (private LAN, loopback, or Tailscale CGNAT).
pub fn is_allowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 127
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

pub fn compute_hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac takes any key");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err("invalid hex length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("hex decode error: {e}")))
        .collect()
}

pub fn format_challenge_payload(nonce: &str, node_id: &str) -> String {
    format!("{}|{}|{}", RELAUNCH_PROTOCOL, nonce, node_id)
}

pub fn verify_signature(key: &[u8], nonce: &str, node_id: &str, signature_hex: &str) -> bool {
    let expected = compute_hmac(key, format_challenge_payload(nonce, node_id).as_bytes());
    let Ok(provided) = hex_decode(signature_hex) else { return false };
    ct_eq(&expected, &provided)
}

/// Operational guardrails: returns Ok(()) only if no active updater lock or deliberate quit sentinel exists.
pub fn can_relaunch(data_dir: &Path) -> Result<(), &'static str> {
    let lock_file = data_dir.join("update").join("installing.lock");
    if lock_file.exists() {
        return Err("update_in_progress");
    }
    let quit_flag = data_dir.join("deliberate_quit.flag");
    if quit_flag.exists() {
        return Err("deliberate_quit");
    }
    Ok(())
}

pub fn parse_wmi_output(text: &str) -> Result<u32, String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("WMI ") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                let code: i32 = parts[1].parse().map_err(|e| format!("bad return code: {e}"))?;
                if code != 0 {
                    return Err(format!("WMI process creation returned error code {code}"));
                }
                let pid: u32 = parts[2].parse().map_err(|e| format!("bad pid: {e}"))?;
                return Ok(pid);
            }
        }
    }
    Err(format!("could not find WMI return status in output: {text}"))
}

#[cfg(windows)]
pub fn relaunch_app(exe_path: &Path) -> Result<u32, String> {
    use std::os::windows::process::CommandExt;
    let exe_str = exe_path.to_str().ok_or("invalid exe path")?;
    let escaped = exe_str.replace('\'', "''");
    let script = format!(
        "$p = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{ CommandLine = '\"{}\"' }}; \
         Write-Output \"WMI $($p.ReturnValue) $($p.ProcessId)\"",
        escaped
    );
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script]);
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = cmd.output().map_err(|e| format!("failed to invoke powershell: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}\n{stderr}");
    parse_wmi_output(&combined)
}

#[cfg(target_os = "macos")]
pub fn relaunch_app(_exe_path: &Path) -> Result<u32, String> {
    let mut cmd = std::process::Command::new("/usr/bin/open");
    cmd.args(["-a", "Consilium"]);
    let status = cmd.status().map_err(|e| format!("failed to invoke open: {e}"))?;
    if status.success() {
        Ok(0)
    } else {
        Err(format!("open command exited with {status}"))
    }
}

#[cfg(all(not(windows), not(target_os = "macos")))]
pub fn relaunch_app(exe_path: &Path) -> Result<u32, String> {
    let target = std::env::var("APPIMAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| exe_path.to_path_buf());
    let child = std::process::Command::new(&target)
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", target.display()))?;
    Ok(child.id())
}

pub async fn send_remote_relaunch(
    peer_addr: SocketAddr,
    key: &[u8],
    my_node_id: &str,
) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    let mut stream = TcpStream::connect(peer_addr)
        .await
        .map_err(|e| format!("failed to connect to watchdog at {peer_addr}: {e}"))?;
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();

    let challenge_line = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .map_err(|_| "timeout waiting for challenge")?
        .map_err(|e| format!("read error: {e}"))?
        .ok_or("connection closed before challenge")?;

    let parts: Vec<&str> = challenge_line.split_whitespace().collect();
    if parts.len() != 2 || parts[0] != "CHALLENGE" {
        return Err(format!("invalid challenge frame: {challenge_line}"));
    }
    let nonce = parts[1];

    let sig = hex_encode(&compute_hmac(key, format_challenge_payload(nonce, my_node_id).as_bytes()));
    let response = format!("RELAUNCH {} {}\n", my_node_id, sig);
    writer.write_all(response.as_bytes()).await.map_err(|e| format!("write error: {e}"))?;
    writer.flush().await.map_err(|e| format!("flush error: {e}"))?;

    let reply_line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
        .await
        .map_err(|_| "timeout waiting for relaunch confirmation")?
        .map_err(|e| format!("read error: {e}"))?
        .ok_or("connection closed before reply")?;

    if reply_line.starts_with("OK") {
        Ok(reply_line)
    } else {
        Err(format!("watchdog rejected relaunch: {reply_line}"))
    }
}

pub type LauncherFn = Arc<dyn Fn(&Path) -> Result<u32, String> + Send + Sync>;

pub async fn run_watchdog_server(
    port: u16,
    data_dir: PathBuf,
    peers: Arc<Mutex<HashMap<String, Vec<u8>>>>,
) -> Result<(), String> {
    run_watchdog_server_with_launcher(port, data_dir, peers, Arc::new(relaunch_app)).await
}

pub async fn run_watchdog_server_with_launcher(
    port: u16,
    data_dir: PathBuf,
    peers: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    launcher: LauncherFn,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|e| format!("watchdog failed to bind port {port}: {e}"))?;

    loop {
        let (mut socket, addr) = match listener.accept().await {
            Ok(res) => res,
            Err(_) => continue,
        };

        if !is_allowed_ip(addr.ip()) {
            let _ = socket.write_all(b"ERR disallowed_ip\n").await;
            continue;
        }

        let peers = peers.clone();
        let data_dir = data_dir.clone();
        let launcher = launcher.clone();

        tokio::spawn(async move {
            let (reader, mut writer) = socket.split();
            let mut lines = BufReader::new(reader).lines();

            let nonce = format!("{}", uuid::Uuid::new_v4().simple());
            if writer.write_all(format!("CHALLENGE {}\n", nonce).as_bytes()).await.is_err() {
                return;
            }
            let _ = writer.flush().await;

            let Ok(Some(line)) = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line()).await
                .map_err(|_| ())
                .and_then(|r| r.map_err(|_| ())) else {
                return;
            };

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() != 3 || parts[0] != "RELAUNCH" {
                let _ = writer.write_all(b"ERR invalid_frame\n").await;
                return;
            }
            let node_id = parts[1];
            let sig_hex = parts[2];

            let key_opt = peers.lock().unwrap().get(node_id).cloned();
            let Some(key) = key_opt else {
                let _ = writer.write_all(b"ERR unknown_node\n").await;
                return;
            };

            if !verify_signature(&key, &nonce, node_id, sig_hex) {
                let _ = writer.write_all(b"ERR bad_signature\n").await;
                return;
            }

            if let Err(reason) = can_relaunch(&data_dir) {
                let _ = writer.write_all(format!("BUSY {}\n", reason).as_bytes()).await;
                return;
            }

            let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("consilium.exe"));
            match launcher(&exe_path) {
                Ok(pid) => {
                    let _ = writer.write_all(format!("OK {}\n", pid).as_bytes()).await;
                }
                Err(e) => {
                    let _ = writer.write_all(format!("ERR {}\n", e).as_bytes()).await;
                }
            }
        });
    }
}

pub fn main_watchdog(_args: &[String]) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("failed to build tokio runtime: {e}");
            return 1;
        }
    };
    let data_dir = crate::data_dir().unwrap_or_else(|| PathBuf::from("."));
    let peers_map = crate::link::load_peer_keys(&data_dir);
    let peers = Arc::new(Mutex::new(peers_map));
    println!("Consilium watchdog listening on port {WATCHDOG_PORT}...");
    if let Err(e) = rt.block_on(run_watchdog_server(WATCHDOG_PORT, data_dir, peers)) {
        eprintln!("watchdog server error: {e}");
        return 1;
    }
    0
}

pub(crate) fn setup_signal_worker(args: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    {
        let target = match args {
            [setup] if setup == "setup" => std::env::current_exe().map_err(|e| e.to_string())?,
            [setup, flag, path] if setup == "setup" && flag == "--app" => {
                std::path::absolute(path).map_err(|e| e.to_string())?
            }
            _ => return Err("usage: consilium watchdog-signal setup".into()),
        };
        validate_target(&target)?;
        let data = crate::data_dir().ok_or("couldn't find Consilium's data folder")?;
        if !crate::signal_watchdog::configured(&data) {
            crate::signal_watchdog::enroll(&data)?;
        }
        crate::signal_watchdog_host::start(&data, &target)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Err("The independent Signal restart group is available on Windows.".into())
    }
}

pub(crate) fn post_signal_commands() -> Result<(), String> {
    let data = crate::data_dir().ok_or("couldn't find Consilium's data folder")?;
    crate::signal_watchdog::post_commands(&data)
}

pub(crate) fn run_power_relay() -> Result<(), String> {
    use crate::signal_watchdog::{Action, Mode};
    use crate::watchdog_power::{PowerConfig, PowerMode};

    let data = crate::data_dir().ok_or("couldn't find Consilium's data folder")?;
    PowerConfig::load(&data)?;
    crate::signal_watchdog::enroll_existing_relay(&data)?;
    crate::signal_watchdog::run_with_mode(&data, Mode::PowerRelay, |action| {
        let Ok(config) = PowerConfig::load(&data) else {
            return "Power control is unconfigured on this relay.".into();
        };
        match action {
            Action::Wake(target) => config.handle(PowerMode::Wake, target),
            Action::Sleep(target) => config.handle(PowerMode::Sleep, target),
            Action::PowerStatus => config.status(),
            _ => "Use the Windows Watchdog for desktop commands.".into(),
        }
    })
}

#[cfg(windows)]
fn validate_target(target: &Path) -> Result<(), String> {
    if target.is_absolute() && target.is_file()
        && target.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.eq_ignore_ascii_case("consilium.exe"))
    {
        Ok(())
    } else {
        Err("couldn't identify the installed Consilium executable".into())
    }
}

pub(crate) fn run_signal_worker(args: &[String]) -> i32 {
    #[cfg(windows)]
    {
        let [target] = args else { return 2 };
        let target = Path::new(target);
        let result = validate_target(target).and_then(|_| {
            let data = crate::data_dir().ok_or("couldn't find Consilium's data folder")?;
            crate::signal_watchdog::run(&data, |action| signal_action(&data, target, action))
        });
        match result {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("Signal watchdog: {error}");
                1
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        2
    }
}

#[cfg(windows)]
fn signal_action(data: &Path, target: &Path, action: crate::signal_watchdog::Action) -> String {
    use crate::restart::windows::{app_pid, remote};
    use crate::signal_watchdog::Action;

    match action {
        Action::RestartApp => match remote(data, target) {
            Ok(()) => "Consilium relaunch requested on Windows.".into(),
            Err(error) => {
                eprintln!("Signal watchdog could not restart Consilium: {error}");
                "Consilium restart could not start. Check the Windows watchdog log.".into()
            }
        },
        Action::RestartSignal => match app_pid(target, data) {
            Ok(Some(_)) => match crate::signal_chat::request_listener_restart(data) {
                Ok(()) => "Consilium Signal listener reconnect requested.".into(),
                Err(_) => "Consilium could not queue a Signal listener reconnect.".into(),
            },
            Ok(None) => "Consilium is closed. Send restart to reopen it and reconnect Signal.".into(),
            Err(_) => "The watchdog could not verify Consilium's app process.".into(),
        },
        Action::Status => match app_pid(target, data) {
            Ok(Some(_)) => "The Signal watchdog is listening. Consilium is running on Windows.".into(),
            Ok(None) => "The Signal watchdog is listening. Consilium is closed on Windows.".into(),
            Err(_) => "The Signal watchdog is listening. Consilium's app status could not be verified.".into(),
        },
        Action::Wake(_) | Action::Sleep(_) | Action::PowerStatus => {
            "Power commands need a configured relay.".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_ips() {
        assert!(is_allowed_ip("127.0.0.1".parse().unwrap()));
        assert!(is_allowed_ip("10.1.2.3".parse().unwrap()));
        assert!(is_allowed_ip("172.20.0.1".parse().unwrap()));
        assert!(is_allowed_ip("192.168.1.100".parse().unwrap()));
        assert!(is_allowed_ip("100.64.0.1".parse().unwrap()));
        assert!(is_allowed_ip("100.100.50.25".parse().unwrap()));
        assert!(is_allowed_ip("::1".parse().unwrap()));

        assert!(!is_allowed_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_allowed_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_allowed_ip("172.32.0.1".parse().unwrap()));
    }

    #[test]
    fn test_challenge_signature_verification() {
        let key_str = format!("{}", uuid::Uuid::new_v4().simple());
        let key = key_str.as_bytes();
        let nonce = format!("{}", uuid::Uuid::new_v4().simple());
        let wrong_nonce = format!("{}", uuid::Uuid::new_v4().simple());
        let node_id = "lukes-mbp";

        let payload = format_challenge_payload(&nonce, node_id);
        let sig = hex_encode(&compute_hmac(key, payload.as_bytes()));

        assert!(verify_signature(key, &nonce, node_id, &sig));

        let wrong_key_str = format!("{}", uuid::Uuid::new_v4().simple());
        let wrong_key = wrong_key_str.as_bytes();
        assert!(!verify_signature(wrong_key, &nonce, node_id, &sig));
        assert!(!verify_signature(key, &wrong_nonce, node_id, &sig));
        assert!(!verify_signature(key, &nonce, "other-node", &sig));
        assert!(!verify_signature(key, &nonce, node_id, "deadbeef"));
    }

    #[test]
    fn test_guardrail_locks() {
        let temp_dir = std::env::temp_dir().join(format!("consilium-watchdog-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        assert_eq!(can_relaunch(&temp_dir), Ok(()));

        let update_dir = temp_dir.join("update");
        std::fs::create_dir_all(&update_dir).unwrap();
        let lock_file = update_dir.join("installing.lock");
        std::fs::write(&lock_file, "locked").unwrap();
        assert_eq!(can_relaunch(&temp_dir), Err("update_in_progress"));

        let _ = std::fs::remove_file(&lock_file);
        assert_eq!(can_relaunch(&temp_dir), Ok(()));

        let quit_flag = temp_dir.join("deliberate_quit.flag");
        std::fs::write(&quit_flag, "quit").unwrap();
        assert_eq!(can_relaunch(&temp_dir), Err("deliberate_quit"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_wmi_output() {
        assert_eq!(parse_wmi_output("WMI 0 5244\r\n"), Ok(5244));
        assert_eq!(parse_wmi_output("Some banner\nWMI 0 1234\nDone"), Ok(1234));
        assert!(parse_wmi_output("WMI 2 0\r\n").unwrap_err().contains("code 2"));
        assert!(parse_wmi_output("Access Denied").is_err());
    }

    #[test]
    fn test_hex_codec() {
        let data = b"hello consilium watchdog";
        let encoded = hex_encode(data);
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(data.to_vec(), decoded);
        assert!(hex_decode("abc").is_err());
    }

    #[tokio::test]
    async fn test_client_server_roundtrip() {
        let key = b"secret_link_key_32_bytes_test!!";
        let mut peers_map = HashMap::new();
        peers_map.insert("test-peer-node".to_string(), key.to_vec());
        let peers = Arc::new(Mutex::new(peers_map));

        let temp_dir = std::env::temp_dir().join(format!("consilium-wt-roundtrip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let data_dir = temp_dir.clone();
        let peers_clone = peers.clone();
        let mock_launcher: LauncherFn = Arc::new(|_p: &Path| Ok(9999));
        tokio::spawn(async move {
            let _ = run_watchdog_server_with_launcher(port, data_dir, peers_clone, mock_launcher).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let res = send_remote_relaunch(addr, key, "test-peer-node").await;
        assert_eq!(res.as_deref(), Ok("OK 9999"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
