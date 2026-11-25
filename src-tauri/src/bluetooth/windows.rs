//! Windows Classic Bluetooth byte streams. Only the selected radio is contacted.
//!
//! A native nonblocking socket stays on one owned thread. Bounded buffers expose
//! AsyncRead/AsyncWrite without routing Bluetooth through TCP or depending on
//! mio's TCP-specific AFD integration. PeerLink performs its normal authentication.
use crate::link::{LinkConnection, RouteEvidence};
use super::stream::{channel, NativePump};
use std::io;
use std::mem::size_of;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use windows_sys::Win32::Devices::Bluetooth::{
    AF_BTH, BTHPROTO_RFCOMM, SOCKADDR_BTH, SOL_RFCOMM, SO_BTH_AUTHENTICATE, SO_BTH_ENCRYPT,
};
use windows_sys::Win32::Networking::WinSock::*;

const IO_CHUNK: usize = 8 * 1024;
const CONNECT_LIMIT: Duration = Duration::from_secs(20);
const WAIT_MS: u32 = 100;

/// Resolves the custom service on exactly `address`, then connects its assigned
/// channel. The bounded discovery worker contacts only that device's service.
pub async fn connect(address: &str, token: CancellationToken) -> Result<LinkConnection, String> {
    let address = parse_address(address)?;
    initialize_winsock()?;
    let wake = Arc::new(Event::new().map_err(|error| error.to_string())?);
    let native_wake = wake.clone();
    let (stream, pump) = channel(Arc::new(move || native_wake.set()));
    let (sender, receiver) = oneshot::channel();
    let worker_token = token.clone();
    std::thread::Builder::new().name("consilium-bluetooth".into()).spawn(move || {
        match open_rfcomm(address, &pump, &wake, &worker_token) {
            Ok((socket, events, evidence)) => {
                let _ = sender.send(Ok(evidence));
                run_socket(socket, events, &pump, &wake, &worker_token);
            }
            Err(error) => {
                pump.finish(Some(error.clone()));
                let _ = sender.send(Err(error));
            }
        }
    }).map_err(|error| format!("Could not start the Bluetooth worker: {error}"))?;
    let evidence = tokio::select! {
        biased;
        _ = token.cancelled() => return Err("Bluetooth connection cancelled.".into()),
        result = receiver => result.map_err(|_| "Bluetooth worker stopped before connecting.".to_string())??,
    };
    Ok(LinkConnection { stream: Box::new(stream), evidence })
}

fn parse_address(address: &str) -> Result<u64, String> {
    if address.len() != 12 || !address.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Select a valid Bluetooth device address.".into());
    }
    let address = u64::from_str_radix(address, 16).map_err(|_| "Invalid Bluetooth device address.")?;
    if address == 0 || address == 0xffff_ffff_ffff {
        return Err("Select an individual Bluetooth device address.".into());
    }
    Ok(address)
}

fn format_address(address: u64) -> String { format!("{:012x}", address & 0xffff_ffff_ffff) }

fn initialize_winsock() -> Result<(), String> {
    // Like std::net, keep one process-lifetime Winsock reference. A per-connection
    // WSACleanup could cancel sockets belonging to other active transports.
    static STARTED: OnceLock<Result<(), i32>> = OnceLock::new();
    STARTED.get_or_init(|| {
        let mut data = WSADATA::default();
        // SAFETY: data is a writable WSADATA and 2.2 is a supported API version.
        let result = unsafe { WSAStartup(0x0202, &mut data) };
        if result == 0 { Ok(()) } else { Err(result) }
    }).map_err(|code| format!("Windows could not initialize Bluetooth sockets ({code})."))
}

struct Event(usize);
impl Event {
    fn new() -> io::Result<Self> {
        // SAFETY: Winsock is initialized before event construction.
        let event = unsafe { WSACreateEvent() };
        if event == 0 { Err(last_error()) } else { Ok(Self(event as usize)) }
    }
    fn handle(&self) -> WSAEVENT { self.0 as WSAEVENT }
    fn set(&self) {
        // SAFETY: the event remains owned for this call; the OS API is thread-safe.
        unsafe { WSASetEvent(self.handle()); }
    }
    fn reset(&self) {
        // SAFETY: the event remains owned for this call.
        unsafe { WSAResetEvent(self.handle()); }
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: this is the event's sole owning handle.
        unsafe { WSACloseEvent(self.handle()); }
    }
}

struct Socket(SOCKET);
impl Drop for Socket {
    fn drop(&mut self) {
        // SAFETY: only this worker uses the nonblocking socket; no I/O is pending.
        unsafe { closesocket(self.0); }
    }
}

fn last_error() -> io::Error {
    // SAFETY: WSAGetLastError reads the current thread's error value.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

fn socket_error(socket: SOCKET) -> io::Result<()> {
    let mut code = 0i32;
    let mut length = size_of::<i32>() as i32;
    // SAFETY: the output points to a writable i32 and has the correct length.
    if unsafe { getsockopt(socket, SOL_SOCKET, SO_ERROR, (&mut code as *mut i32).cast(), &mut length) } != 0 {
        return Err(last_error());
    }
    if code == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(code)) }
}

fn bluetooth_error(error: io::Error) -> String {
    match error.raw_os_error() {
        Some(WSAEACCES) => "Bluetooth authentication failed. Pair these computers in their Bluetooth settings and confirm the matching code.".into(),
        Some(WSAEADDRNOTAVAIL) => "The Mac's Bluetooth address or Consilium RFCOMM channel was unavailable (10049). Restart Bluetooth in Consilium on the Mac, then retest.".into(),
        Some(WSAETIMEDOUT | WSAEHOSTDOWN | WSAENETUNREACH) => "The selected computer's Bluetooth service did not answer. Keep Consilium open on that computer and check its Bluetooth connection.".into(),
        Some(code) => format!("Windows Bluetooth connection failed ({code})."),
        None => error.to_string(),
    }
}

fn open_rfcomm(address: u64, pump: &NativePump, wake: &Event, token: &CancellationToken) -> Result<(Socket, Event, RouteEvidence), String> {
    let cancelled = || pump.is_closed() || token.is_cancelled();
    let deadline = Instant::now() + CONNECT_LIMIT;
    let channel = super::sdp::resolve_channel(address, deadline, cancelled)?;
    if cancelled() { return Err("Bluetooth connection cancelled.".into()); }
    if Instant::now() >= deadline { return Err("Bluetooth connection timed out.".into()); }
    // SAFETY: these parameters select the native Windows RFCOMM provider.
    let raw = unsafe { socket(AF_BTH as i32, SOCK_STREAM, BTHPROTO_RFCOMM as i32) };
    if raw == INVALID_SOCKET { return Err(bluetooth_error(last_error())); }
    let socket = Socket(raw);
    let enabled = 1u32;
    for option in [SO_BTH_AUTHENTICATE, SO_BTH_ENCRYPT] {
        // SAFETY: options require a pointer to ULONG TRUE and sizeof(ULONG).
        if unsafe { setsockopt(raw, SOL_RFCOMM as i32, option as i32, (&enabled as *const u32).cast(), size_of::<u32>() as i32) } != 0 {
            return Err(bluetooth_error(last_error()));
        }
    }
    let events = attach_events(raw).map_err(bluetooth_error)?;
    let remote = SOCKADDR_BTH {
        addressFamily: AF_BTH,
        btAddr: address,
        // Some providers return port zero in getpeername after a UUID-only dial.
        // Use the exact channel discovered for this device and service instead.
        serviceClassId: windows_sys::core::GUID::from_u128(0),
        port: channel,
    };
    // SAFETY: remote is a complete packed SOCKADDR_BTH. WSAEventSelect made the
    // socket nonblocking, including the connect operation.
    let result = unsafe { windows_sys::Win32::Networking::WinSock::connect(raw, (&remote as *const SOCKADDR_BTH).cast(), size_of::<SOCKADDR_BTH>() as i32) };
    if result != 0 {
        let error = last_error();
        if !matches!(error.raw_os_error(), Some(WSAEWOULDBLOCK | WSAEINPROGRESS | WSAEALREADY)) {
            return Err(bluetooth_error(error));
        }
        loop {
            if cancelled() { return Err("Bluetooth connection cancelled.".into()); }
            if Instant::now() >= deadline { return Err("Bluetooth connection timed out while opening the selected RFCOMM channel.".into()); }
            let network = wait_events(raw, &events, wake).map_err(bluetooth_error)?;
            if network.lNetworkEvents & FD_CONNECT as i32 != 0 {
                let error = network.iErrorCode[FD_CONNECT_BIT as usize];
                if error != 0 { return Err(bluetooth_error(io::Error::from_raw_os_error(error))); }
                break;
            }
            if network.lNetworkEvents & FD_CLOSE as i32 != 0 {
                return Err("The remote Bluetooth service closed before connecting.".into());
            }
        }
    }
    socket_error(raw).map_err(bluetooth_error)?;
    let local = socket_address(raw, false).map_err(bluetooth_error)?;
    let peer = socket_address(raw, true).map_err(bluetooth_error)?;
    let channel = verified_channel(peer, address, channel)?;
    let evidence = RouteEvidence::BluetoothRfcomm {
        local_adapter: format_address(local.btAddr),
        remote_device: format_address(peer.btAddr),
        service_uuid: super::SERVICE_UUID.into(),
        channel,
    };
    Ok((socket, events, evidence))
}

fn verified_channel(peer: SOCKADDR_BTH, address: u64, resolved: u32) -> Result<u8, String> {
    // Copy packed fields by value before checking the socket's observed route.
    let peer_address = peer.btAddr;
    let port = peer.port;
    if peer_address != address || port != resolved || !(1..=30).contains(&port) {
        return Err("The connected Bluetooth radio or RFCOMM channel did not match the selected service.".into());
    }
    Ok(port as u8)
}

fn socket_address(socket: SOCKET, peer: bool) -> io::Result<SOCKADDR_BTH> {
    let mut address = SOCKADDR_BTH::default();
    let mut length = size_of::<SOCKADDR_BTH>() as i32;
    // SAFETY: address is a writable SOCKADDR_BTH and length describes its allocation.
    let result = unsafe {
        if peer { getpeername(socket, (&mut address as *mut SOCKADDR_BTH).cast(), &mut length) }
        else { getsockname(socket, (&mut address as *mut SOCKADDR_BTH).cast(), &mut length) }
    };
    if result != 0 { return Err(last_error()); }
    if length as usize != size_of::<SOCKADDR_BTH>() || address.addressFamily != AF_BTH {
        return Err(io::Error::other("Windows returned invalid Bluetooth route evidence."));
    }
    Ok(address)
}

fn attach_events(socket: SOCKET) -> io::Result<Event> {
    let event = Event::new()?;
    // SAFETY: socket/event are live. Event selection makes the socket nonblocking.
    let result = unsafe { WSAEventSelect(socket, event.handle(), (FD_CONNECT | FD_READ | FD_WRITE | FD_CLOSE) as i32) };
    if result != 0 { Err(last_error()) } else { Ok(event) }
}

fn wait_events(socket: SOCKET, network: &Event, wake: &Event) -> io::Result<WSANETWORKEVENTS> {
    let handles = [network.handle() as _, wake.handle() as _];
    // SAFETY: both handles are live and the count matches the array.
    let result = unsafe { WSAWaitForMultipleEvents(2, handles.as_ptr(), 0, WAIT_MS, 0) };
    if result == WSA_WAIT_FAILED { return Err(last_error()); }
    wake.reset();
    let mut events = WSANETWORKEVENTS::default();
    if result == WSA_WAIT_EVENT_0 as u32 {
        // SAFETY: the owned socket/event pair has a writable event record.
        if unsafe { WSAEnumNetworkEvents(socket, network.handle(), &mut events) } != 0 { return Err(last_error()); }
    }
    Ok(events)
}

fn run_socket(socket: Socket, events: Event, pump: &NativePump, wake: &Event, token: &CancellationToken) {
    let outcome = (|| -> io::Result<()> {
        loop {
            if pump.is_closed() || token.is_cancelled() { return Err(io::Error::new(io::ErrorKind::Interrupted, "Bluetooth connection cancelled.")); }
            if pump_read(socket.0, pump)? { return Ok(()); }
            pump_write(socket.0, pump)?;
            if pump.shutdown_requested() && pump.shutdown_ready() {
                // RFCOMM has no TCP half-close: after queued bytes are accepted,
                // shutdown disconnects the channel in both directions.
                // SAFETY: the nonblocking socket is owned by this worker.
                if unsafe { shutdown(socket.0, SD_BOTH) } != 0 { return Err(last_error()); }
                pump.complete_shutdown();
                return Ok(());
            }
            let network = wait_events(socket.0, &events, wake)?;
            for (flag, bit) in [(FD_READ, FD_READ_BIT), (FD_WRITE, FD_WRITE_BIT), (FD_CLOSE, FD_CLOSE_BIT)] {
                let error = network.iErrorCode[bit as usize];
                if network.lNetworkEvents & flag as i32 != 0 && error != 0 { return Err(io::Error::from_raw_os_error(error)); }
            }
        }
    })();
    pump.finish(outcome.err().map(|error| error.to_string()));
    // socket and event are dropped here, on their sole owner thread.
}

fn pump_read(socket: SOCKET, pump: &NativePump) -> io::Result<bool> {
    let mut chunk = [0u8; IO_CHUNK];
    // Bound work per turn so continuous traffic cannot starve writes/cancellation.
    for _ in 0..8 {
        let room = pump.read_capacity().min(chunk.len());
        if room == 0 { return Ok(false); }
        // SAFETY: socket is nonblocking and chunk has room bytes of writable space.
        let received = unsafe { recv(socket, chunk.as_mut_ptr(), room as i32, 0) };
        if received == SOCKET_ERROR {
            let error = last_error();
            return if error.raw_os_error() == Some(WSAEWOULDBLOCK) { Ok(false) } else { Err(error) };
        }
        if received == 0 {
            pump.end_read();
            return Ok(true);
        }
        pump.incoming(&chunk[..received as usize])?;
    }
    Ok(false)
}

fn pump_write(socket: SOCKET, pump: &NativePump) -> io::Result<()> {
    for _ in 0..8 {
        let Some(chunk) = pump.outgoing(IO_CHUNK) else { return Ok(()) };
        // SAFETY: socket is nonblocking and chunk contains initialized bytes.
        let sent = unsafe { send(socket, chunk.as_ptr(), chunk.len() as i32, 0) };
        if sent == SOCKET_ERROR {
            let error = last_error();
            return if error.raw_os_error() == Some(WSAEWOULDBLOCK) { Ok(()) } else { Err(error) };
        }
        if sent == 0 { return Err(io::ErrorKind::WriteZero.into()); }
        pump.consume_written(sent as usize)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_evidence_requires_the_exact_resolved_channel_and_radio() {
        let address = 0x020000000001;
        let peer = |bt_addr, port| SOCKADDR_BTH {
            addressFamily: AF_BTH,
            btAddr: bt_addr,
            port,
            ..Default::default()
        };
        assert_eq!(verified_channel(peer(address, 1), address, 1), Ok(1));
        assert!(verified_channel(peer(address, 0), address, 1).is_err());
        assert!(verified_channel(peer(address, 2), address, 1).is_err());
        assert!(verified_channel(peer(address + 1, 1), address, 1).is_err());
        assert!(verified_channel(peer(address, 31), address, 31).is_err());
    }
    use super::super::stream::NativeStream;
    use std::os::windows::io::IntoRawSocket;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Exercises the exact nonblocking Winsock pump through owned loopback TCP
    /// sockets, without querying radios, pairing, or claiming Bluetooth proof.
    fn native_socket_fixture() -> (NativeStream, tokio::net::TcpStream, CancellationToken, oneshot::Receiver<()>) {
        initialize_winsock().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_nonblocking(true).unwrap();
        let peer = tokio::net::TcpStream::from_std(peer).unwrap();
        let socket = Socket(client.into_raw_socket() as SOCKET);
        let events = attach_events(socket.0).unwrap();
        let wake = Arc::new(Event::new().unwrap());
        let native_wake = wake.clone();
        let (stream, pump) = channel(Arc::new(move || native_wake.set()));
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let (finished, receiver) = oneshot::channel();
        std::thread::spawn(move || {
            run_socket(socket, events, &pump, &wake, &worker_token);
            let _ = finished.send(());
        });
        (stream, peer, token, receiver)
    }

    #[test]
    fn device_addresses_never_accept_hosts_commands_or_broadcast_targets() {
        assert_eq!(parse_address("001122aAbBcC").unwrap(), 0x0011_22aa_bbcc);
        for invalid in ["", "000000000000", "ffffffffffff", "127.0.0.1", "00:11:22:AA:BB:CC", "rfcomm://001122aabbcc", " 001122aabbcc", "001122aabbcc;", "001122aabbcg"] {
            assert!(parse_address(invalid).is_err(), "Invalid device address was accepted");
        }
    }

    #[tokio::test]
    async fn native_winsock_pump_preserves_large_bidirectional_data_then_full_shutdown() {
        let (mut stream, mut peer, _, finished) = native_socket_fixture();
        let payload: Vec<u8> = (0..256 * 1024).map(|index| (index % 251) as u8).collect();
        let reply: Vec<u8> = payload.iter().rev().copied().collect();
        tokio::time::timeout(Duration::from_secs(15), async {
            let client = async {
                stream.write_all(&payload).await.unwrap();
                stream.flush().await.unwrap();
                let mut received = vec![0; reply.len()];
                stream.read_exact(&mut received).await.unwrap();
                assert_eq!(received, reply);
                stream.shutdown().await.unwrap();
                let mut tail = Vec::new();
                stream.read_to_end(&mut tail).await.unwrap();
                assert!(tail.is_empty());
            };
            let server = async {
                let mut received = vec![0; payload.len()];
                peer.read_exact(&mut received).await.unwrap();
                assert_eq!(received, payload);
                peer.write_all(&reply).await.unwrap();
                peer.flush().await.unwrap();
                let mut tail = Vec::new();
                peer.read_to_end(&mut tail).await.unwrap();
                assert!(tail.is_empty());
            };
            tokio::join!(client, server);
            finished.await.unwrap();
        }).await.expect("The native event-loop pump must make progress in both directions");
    }

    #[tokio::test]
    async fn cancelling_an_idle_native_socket_closes_and_reaps_its_worker() {
        let (mut stream, mut peer, token, finished) = native_socket_fixture();
        token.cancel();
        tokio::time::timeout(Duration::from_secs(2), finished).await.unwrap().unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
        assert_eq!(tokio::time::timeout(Duration::from_secs(2), peer.read(&mut byte)).await.unwrap().unwrap(), 0);
    }

    #[tokio::test]
    async fn dropping_the_stream_closes_an_idle_native_socket_without_a_live_tokio_task() {
        let (stream, mut peer, _, finished) = native_socket_fixture();
        drop(stream);
        tokio::time::timeout(Duration::from_secs(2), finished).await.unwrap().unwrap();
        let mut byte = [0];
        assert_eq!(tokio::time::timeout(Duration::from_secs(2), peer.read(&mut byte)).await.unwrap().unwrap(), 0);
    }

    #[tokio::test]
    async fn native_remote_close_preserves_buffered_bytes_then_closes_its_worker() {
        let (mut stream, mut peer, _, finished) = native_socket_fixture();
        tokio::time::timeout(Duration::from_secs(5), async {
            peer.write_all(b"last native bytes").await.unwrap();
            peer.shutdown().await.unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, b"last native bytes");
            assert_eq!(stream.write_all(b"late").await.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
            finished.await.unwrap();
        }).await.unwrap();
    }
}
