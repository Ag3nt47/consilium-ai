//! Fixed power operations for workstations selected in local watchdog configuration.

use crate::watchdog_sleep::SCHEDULED as SLEEP_ACK;
use serde::{Deserialize, Deserializer};
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CONFIG_NAME: &str = "watchdog-power.json";
const CONFIG_LIMIT: u64 = 16 * 1024;
const WAKE_PORT: u16 = 9;
const SLEEP_COMMAND: &str = "consilium watchdog-signal sleep-local";
const SSH_DEADLINE: Duration = Duration::from_secs(12);
const MAX_SSH_OUTPUT: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PowerTarget {
    Windows,
    Mac,
    Linux,
    Both,
}

impl PowerTarget {
    fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Mac => "Mac",
            Self::Linux => "Linux",
            Self::Both => "Both",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PowerMode {
    Sleep,
    Wake,
    Status,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PowerConfig {
    ssh_config: Option<PathBuf>,
    windows: Option<TargetConfig>,
    mac: Option<TargetConfig>,
    linux: Option<TargetConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetConfig {
    wake: Option<WakeConfig>,
    ssh_alias: Option<String>,
    status_endpoint: Option<SocketAddrV4>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WakeConfig {
    #[serde(deserialize_with = "deserialize_mac")]
    mac: [u8; 6],
    source_ipv4: Ipv4Addr,
}

impl PowerConfig {
    pub(crate) fn load(data: &Path) -> Result<Self, String> {
        let path = data.join(CONFIG_NAME);
        let file = File::open(path).map_err(|e| match e.kind() {
            ErrorKind::NotFound => "Set up watchdog-power.json on the watchdog computer first.",
            _ => "Could not read the local watchdog power configuration.",
        })?;
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Err("The watchdog power configuration must be a regular file.".into());
        }
        let mut bytes = Vec::new();
        file.take(CONFIG_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read the local watchdog power configuration.")?;
        if bytes.len() as u64 > CONFIG_LIMIT {
            return Err("The watchdog power configuration is too large.".into());
        }
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|_| "The local watchdog power configuration is invalid.")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        let mut aliases = Vec::new();
        for target in self.windows.iter().chain(&self.mac).chain(&self.linux) {
            if let Some(wake) = &target.wake {
                if !wake.source_ipv4.is_private() {
                    return Err("Wake requires a private local IPv4 adapter address.".into());
                }
            }
            if let Some(alias) = &target.ssh_alias {
                if !valid_alias(alias)
                    || aliases
                        .iter()
                        .any(|other: &&str| other.eq_ignore_ascii_case(alias))
                {
                    return Err("Each SSH target must have a distinct local OpenSSH alias.".into());
                }
                aliases.push(alias.as_str());
            }
            if let Some(endpoint) = &target.status_endpoint {
                if !endpoint.ip().is_private() || endpoint.port() == 0 {
                    return Err(
                        "A watchdog status endpoint needs a private IPv4 address and TCP port."
                            .into(),
                    );
                }
            }
        }
        if !aliases.is_empty()
            && !self
                .ssh_config
                .as_ref()
                .is_some_and(|path| path.is_absolute())
        {
            return Err(
                "Set an absolute path to a trusted OpenSSH config file for sleep commands.".into(),
            );
        }
        Ok(())
    }

    fn target(&self, target: PowerTarget) -> Option<&TargetConfig> {
        match target {
            PowerTarget::Windows => self.windows.as_ref(),
            PowerTarget::Mac => self.mac.as_ref(),
            PowerTarget::Linux => self.linux.as_ref(),
            PowerTarget::Both => None,
        }
    }

    pub(crate) fn handle(&self, mode: PowerMode, target: PowerTarget) -> String {
        if target == PowerTarget::Both {
            let unix = match (&self.mac, &self.linux) {
                (Some(_), None) => PowerTarget::Mac,
                (None, Some(_)) => PowerTarget::Linux,
                _ => {
                    return "Both requires a Windows target and exactly one Mac or Linux target."
                        .into()
                }
            };
            if self.windows.is_none() {
                return "Both requires a Windows target and exactly one Mac or Linux target."
                    .into();
            }
            // The Windows watchdog can be one of the two targets. Request the other computer's
            // sleep first, so local sleep cannot interrupt that request or its acknowledgement.
            let unix_reply = self.handle(mode, unix);
            return format!("{}\n{unix_reply}", self.handle(mode, PowerTarget::Windows));
        }
        let result = self
            .target(target)
            .ok_or("Power control is unconfigured for this target.")
            .and_then(|cfg| match mode {
                PowerMode::Wake => cfg
                    .wake
                    .as_ref()
                    .ok_or("Wake is unconfigured.")
                    .and_then(send_magic_packet),
                PowerMode::Sleep => self
                    .ssh_config
                    .as_deref()
                    .zip(cfg.ssh_alias.as_deref())
                    .ok_or("Remote sleep is unconfigured.")
                    .and_then(|(config, alias)| request_sleep(config, alias)),
                PowerMode::Status => cfg
                    .status_endpoint
                    .ok_or("TCP status is unconfigured.")
                    .map(read_status),
            });
        format!(
            "{} — {}",
            target.label(),
            result.unwrap_or_else(|error| error)
        )
    }

    pub(crate) fn status(&self) -> String {
        let summary = [PowerTarget::Windows, PowerTarget::Mac, PowerTarget::Linux]
            .into_iter()
            .filter(|target| self.target(*target).is_some())
            .map(|target| self.handle(PowerMode::Status, target))
            .collect::<Vec<_>>()
            .join("\n");
        if summary.is_empty() {
            "The watchdog relay has no configured computers.".into()
        } else {
            summary
        }
    }
}

fn valid_alias(alias: &str) -> bool {
    let bytes = alias.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn deserialize_mac<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 6], D::Error> {
    let text = String::deserialize(d)?;
    parse_mac(&text)
        .ok_or_else(|| serde::de::Error::custom("expected one unicast Ethernet MAC address"))
}

fn parse_mac(text: &str) -> Option<[u8; 6]> {
    if !text.is_ascii() {
        return None;
    }
    let stride = match text.len() {
        12 => 2,
        17 => {
            let separator = text.as_bytes()[2];
            if !matches!(separator, b':' | b'-')
                || ![2, 5, 8, 11, 14]
                    .into_iter()
                    .all(|i| text.as_bytes()[i] == separator)
            {
                return None;
            }
            3
        }
        _ => return None,
    };
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        let pair = &text[i * stride..i * stride + 2];
        if !pair.as_bytes().iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    (mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
}

fn private_broadcast(source: Ipv4Addr, netmask: Ipv4Addr) -> Option<Ipv4Addr> {
    let mask = u32::from(netmask);
    let prefix = mask.leading_ones();
    if !source.is_private() || !(8..=30).contains(&prefix) || mask != u32::MAX << (32 - prefix) {
        return None;
    }
    let host = u32::from(source) & !mask;
    let broadcast = Ipv4Addr::from(u32::from(source) | !mask);
    (host != 0 && host != !mask && broadcast.is_private()).then_some(broadcast)
}

fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xff; 102];
    let (addresses, _) = packet[6..].as_chunks_mut::<6>();
    for target in addresses {
        *target = mac;
    }
    packet
}

fn send_magic_packet(wake: &WakeConfig) -> Result<&'static str, &'static str> {
    let interfaces = if_addrs::get_if_addrs().map_err(|_| "Could not read the wake adapter.")?;
    let mut broadcasts = interfaces
        .iter()
        .filter_map(|interface| match &interface.addr {
            if_addrs::IfAddr::V4(address) if address.ip == wake.source_ipv4 => {
                private_broadcast(address.ip, address.netmask)
            }
            _ => None,
        });
    let broadcast = broadcasts
        .next()
        .filter(|_| broadcasts.next().is_none())
        .ok_or("The configured wake adapter is unavailable or ambiguous.")?;
    let socket = UdpSocket::bind(SocketAddrV4::new(wake.source_ipv4, 0))
        .map_err(|_| "Could not use the configured wake adapter.")?;
    socket
        .set_broadcast(true)
        .map_err(|_| "The wake adapter could not enable broadcast.")?;
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "Could not set the wake packet timeout.")?;
    let packet = magic_packet(wake.mac);
    let sent = socket
        .send_to(&packet, SocketAddrV4::new(broadcast, WAKE_PORT))
        .map_err(|_| "Could not send the wake packet.")?;
    if sent != packet.len() {
        return Err("Could not send the complete wake packet.");
    }
    Ok("Wake packet sent. Power state unconfirmed.")
}

fn read_status(endpoint: SocketAddrV4) -> &'static str {
    if TcpStream::connect_timeout(&endpoint.into(), Duration::from_secs(2)).is_ok() {
        "The configured TCP endpoint is reachable."
    } else {
        "The configured TCP endpoint is unavailable. Power state unconfirmed."
    }
}

fn openssh_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let path = PathBuf::from(std::env::var_os("SystemRoot")?).join("System32\\OpenSSH\\ssh.exe");
    #[cfg(not(windows))]
    let path = PathBuf::from("/usr/bin/ssh");
    (path.is_absolute() && path.is_file()).then_some(path)
}

fn sleep_command(ssh: &Path, config: &Path, alias: &str) -> Command {
    let mut command = Command::new(ssh);
    command.args(["-T", "-n", "-S", "none", "-F"]).arg(config);
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ConnectionAttempts=1",
        "ConnectTimeout=5",
        "ClearAllForwardings=yes",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "ControlMaster=no",
        "ProxyCommand=none",
        "ProxyJump=none",
        "CanonicalizeHostname=no",
        "PreferredAuthentications=publickey",
        "IdentitiesOnly=yes",
    ] {
        command.arg("-o").arg(option);
    }
    command
        .arg(alias)
        .arg(SLEEP_COMMAND)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

fn has_sleep_ack(bytes: &[u8]) -> bool {
    bytes.len() as u64 <= MAX_SSH_OUTPUT
        && std::str::from_utf8(bytes).is_ok_and(|text| text.lines().any(|line| line == SLEEP_ACK))
}

fn request_sleep(config: &Path, alias: &str) -> Result<&'static str, &'static str> {
    if !config.is_file() {
        return Err("The configured OpenSSH file is unavailable.");
    }
    let ssh = openssh_path().ok_or("Install the system OpenSSH client for remote sleep.")?;
    let mut child = sleep_command(&ssh, config, alias)
        .spawn()
        .map_err(|_| "Could not start the OpenSSH sleep request.")?;
    let stdout = child
        .stdout
        .take()
        .expect("OpenSSH output was configured as a pipe");
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::Builder::new().spawn(move || {
        let mut output = Vec::new();
        let _ = tx.send(
            stdout
                .take(MAX_SSH_OUTPUT + 1)
                .read_to_end(&mut output)
                .map(|_| output),
        );
    });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Could not read the OpenSSH sleep acknowledgement.");
    }
    let deadline = Instant::now() + SSH_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let acknowledged = rx
        .recv_timeout(Duration::from_millis(300))
        .ok()
        .and_then(Result::ok)
        .is_some_and(|bytes| has_sleep_ack(&bytes));
    if acknowledged {
        Ok("Sleep scheduled by the target. Standby state unconfirmed.")
    } else {
        Err("No sleep acknowledgement received. Standby state unconfirmed.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_packet_repeats_a_valid_unicast_mac_sixteen_times() {
        let mac = [2, 100, 16, 131, 52, 25];
        assert_eq!(parse_mac("02:64:10:83:34:19"), Some(mac));
        assert_eq!(parse_mac("02-64-10-83-34-19"), Some(mac));
        assert_eq!(parse_mac("026410833419"), Some(mac));
        for invalid in [
            "01:64:10:83:34:19",
            "FF:FF:FF:FF:FF:FF",
            "00:00:00:00:00:00",
            "02:64-10:83:34:19",
            "+26410833419",
        ] {
            assert_eq!(parse_mac(invalid), None);
        }
        let packet = magic_packet(mac);
        assert_eq!(&packet[..6], &[0xff; 6]);
        assert_eq!(packet[6..].chunks_exact(6).count(), 16);
        assert!(packet[6..].chunks_exact(6).all(|part| part == mac));
    }

    #[test]
    fn wake_broadcast_uses_the_adapter_subnet_and_stays_private() {
        let address: Ipv4Addr = "192.168.1.74".parse().unwrap();
        assert_eq!(
            private_broadcast(address, "255.255.255.128".parse().unwrap()),
            Some("192.168.1.127".parse().unwrap())
        );
        for mask in ["0.0.0.0", "255.255.253.0", "255.255.255.255"] {
            assert_eq!(private_broadcast(address, mask.parse().unwrap()), None);
        }
        assert_eq!(
            private_broadcast("8.8.8.1".parse().unwrap(), "255.255.255.0".parse().unwrap()),
            None
        );
    }

    #[test]
    fn only_fixed_sleep_command_and_restricted_aliases_are_used() {
        for invalid in [
            "",
            "-F",
            "user@example.com",
            "ssh://host",
            "two names",
            "mac;echo",
            "mac\ncommand",
            "💻",
        ] {
            assert!(!valid_alias(invalid));
        }
        let alias = "consilium-mac";
        assert!(valid_alias(alias));
        let command = sleep_command(
            Path::new("ssh"),
            Path::new("local trusted ssh config"),
            alias,
        );
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[args.len() - 2], alias);
        assert_eq!(
            args[args.len() - 1],
            "consilium watchdog-signal sleep-local"
        );
        assert!(args.contains(&"StrictHostKeyChecking=yes".as_ref()));
        assert!(has_sleep_ack(b"Consilium sleep scheduled\r\n"));
        assert!(!has_sleep_ack(b"Consilium sleep scheduled but rejected\n"));
    }

    #[test]
    fn power_config_rejects_public_endpoints_and_both_stays_unambiguous() {
        let config: PowerConfig =
            serde_json::from_str(r#"{"windows":{"status_endpoint":"8.8.8.8:22"}}"#).unwrap();
        assert!(config.validate().is_err());
        let config: PowerConfig =
            serde_json::from_str(r#"{"windows":{},"mac":{},"linux":{}}"#).unwrap();
        assert!(config
            .handle(PowerMode::Status, PowerTarget::Both)
            .contains("exactly one"));
        let config: PowerConfig = serde_json::from_str(r#"{"windows":{},"mac":{}}"#).unwrap();
        assert_eq!(
            config
                .handle(PowerMode::Status, PowerTarget::Both)
                .lines()
                .count(),
            2
        );
        assert!(serde_json::from_str::<PowerConfig>(r#"{"windows":{"command":"sleep"}}"#).is_err());
    }
}
