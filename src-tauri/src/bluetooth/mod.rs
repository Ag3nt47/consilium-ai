//! Optional native Bluetooth transport. The operating system owns pairing; PeerLink
//! still authenticates and encrypts every connection over the resulting byte stream.
use crate::link::LinkConnection;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use crate::link::LinkStream;
#[cfg(windows)]
use std::pin::Pin;
use std::sync::OnceLock;
#[cfg(windows)]
use std::task::{Context, Poll};
#[cfg(windows)]
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

mod controller;
pub mod pairing;
#[cfg(windows)]
mod pairing_worker;
use controller::Controller;
#[cfg(windows)]
use controller::SessionRelay;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, target_os = "macos", test))]
mod stream;

pub const SERVICE_UUID: &str = "6c4d9403-81a6-4ab0-98b8-57fba7325481";
static CONTROL: OnceLock<Controller> = OnceLock::new();

#[derive(Default, Deserialize, Serialize)]
struct Settings {
    enabled: bool,
}

#[derive(Clone, Serialize)]
pub struct Status {
    pub supported: bool,
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub detail: String,
}

fn control() -> &'static Controller { CONTROL.get_or_init(Controller::new) }

pub fn initialize(app: tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    macos::initialize(app);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    let settings = crate::data_dir()
        .and_then(|dir| std::fs::read(dir.join("native-bluetooth.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .unwrap_or_default();
    control().initialize(settings.enabled && cfg!(any(windows, target_os = "macos")));
}

pub fn is_enabled() -> bool {
    control().is_enabled()
}

pub fn status() -> Status {
    let supported = cfg!(any(windows, target_os = "macos"));
    let (enabled, failure) = control().snapshot();
    let endpoint = local_endpoint();
    let detail = if !supported {
        "Native Bluetooth is available on Windows and macOS.".into()
    } else if !enabled {
        "Bluetooth is off in Consilium. Enable it on both paired computers to use their native Bluetooth connection.".into()
    } else if let Some(failure) = failure {
        failure
    } else if endpoint.is_some() {
        "Bluetooth is ready for a paired Windows computer. PeerLink still verifies each connection.".into()
    } else if cfg!(windows) {
        "Ready to connect to a paired Mac with Consilium Bluetooth enabled. Keep Ethernet connected while the Mac announces its Bluetooth address.".into()
    } else {
        "Starting Bluetooth. Allow Bluetooth access if macOS asks, and pair the computers in their Bluetooth settings.".into()
    };
    Status { supported, enabled, endpoint, detail }
}

pub fn set_enabled(enabled: bool) -> Result<Status, String> {
    if enabled && !cfg!(any(windows, target_os = "macos")) {
        return Err("Native Bluetooth is available on Windows and macOS.".into());
    }
    let dir = crate::data_dir().ok_or("The Consilium data folder is unavailable")?;
    let json = serde_json::to_vec(&Settings { enabled }).map_err(|e| e.to_string())?;
    control().set_enabled(enabled, || {
        std::fs::create_dir_all(&dir).map_err(|e| format!("Save Bluetooth setting: {e}"))?;
        crate::store::write_atomic(&dir.join("native-bluetooth.json"), &json)
            .map_err(|e| format!("Save Bluetooth setting: {e}"))
    })?;
    Ok(status())
}

/// Strict endpoints are safe to advertise only inside an authenticated PeerLink hello.
pub fn device_address(endpoint: &str) -> Option<String> {
    let address = endpoint.strip_prefix("rfcomm://")?;
    (address.len() == 12 && address.bytes().all(|b| b.is_ascii_hexdigit())
        && address != "000000000000" && !address.eq_ignore_ascii_case("FFFFFFFFFFFF"))
        .then(|| address.to_ascii_uppercase())
}

pub fn local_endpoint() -> Option<String> {
    if !is_enabled() || !control().listener_ready() { return None; }
    #[cfg(target_os = "macos")]
    return macos::local_endpoint().filter(|endpoint| device_address(endpoint).is_some());
    #[cfg(not(target_os = "macos"))]
    None
}

pub async fn listen(accepted: mpsc::Sender<LinkConnection>, token: CancellationToken) -> Result<(), String> {
    controller::run_listener(control(), token, move |stop, ready| {
        let accepted = accepted.clone();
        async move {
            #[cfg(target_os = "macos")]
            { macos::listen(accepted, stop, ready).await }
            #[cfg(not(target_os = "macos"))]
            { let _ = (accepted, ready); stop.cancelled().await; Ok(()) }
        }
    }).await
}

pub async fn connect(address: &str, token: CancellationToken) -> Result<LinkConnection, String> {
    let address = device_address(address)
        .or_else(|| device_address(&format!("rfcomm://{address}")))
        .ok_or("Invalid Bluetooth device address")?;
    if !is_enabled() { return Err("Enable native Bluetooth in Consilium on both computers first.".into()); }
    if token.is_cancelled() { return Err("Bluetooth connection cancelled.".into()); }
    #[cfg(windows)]
    {
        let stop = control().active_epoch().ok_or("Native Bluetooth was disabled before the connection started.")?.token;
        let relay = SessionRelay::new(token.clone(), stop.clone());
        let mut connection = windows::connect(&address, stop.clone()).await?;
        if stop.is_cancelled() || token.is_cancelled() {
            return Err("Bluetooth was disabled or the connection was cancelled.".into());
        }
        connection.stream = Box::new(ManagedStream { _relay: relay, stream: connection.stream });
        Ok(connection)
    }
    #[cfg(not(windows))]
    { let _ = (address, token); Err("This computer receives Bluetooth connections from a paired Windows PC.".into()) }
}

#[cfg(windows)]
struct ManagedStream {
    // Drop cancellation before the underlying stream, including pending connects.
    _relay: SessionRelay,
    stream: LinkStream,
}

#[cfg(windows)]
impl AsyncRead for ManagedStream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

#[cfg(windows)]
impl AsyncWrite for ManagedStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> { Pin::new(&mut self.stream).poll_write(cx, buf) }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.stream).poll_flush(cx) }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.stream).poll_shutdown(cx) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn endpoint_requires_one_canonical_device_without_path_or_channel() {
        assert_eq!(super::device_address("rfcomm://A1b2C3d4E5f6").as_deref(), Some("A1B2C3D4E5F6"));
        for invalid in ["rfcomm://", "rfcomm://A1:B2:C3:D4:E5:F6", "rfcomm://A1B2C3D4E5F6/1", "rfcomm://A1B2C3D4E5F6?channel=1", "rfcomm://000000000000", "rfcomm://FFFFFFFFFFFF", "rfcomm://A1B2C3D4E5FG", "A1B2C3D4E5F6", "https://A1B2C3D4E5F6"] {
            assert!(super::device_address(invalid).is_none(), "accepted {invalid}");
        }
    }
}
