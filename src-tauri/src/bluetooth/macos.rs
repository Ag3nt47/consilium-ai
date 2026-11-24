//! IOBluetooth's RFCOMM server. Native objects never leave the Tauri main thread.
//!
//! A transient SDP record advertises only our custom PeerLink service. OS pairing
//! remains the user's responsibility; the existing PeerLink handshake authenticates
//! the byte stream before it can carry any application messages.
use super::stream::{channel as byte_channel, NativePump};
use super::SERVICE_UUID;
use crate::link::{LinkConnection, RouteEvidence};
use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_foundation::{ns_string, NSArray, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};
use objc2_io_bluetooth::{
    BluetoothHCIPowerState, IOBluetoothHostController,
    IOBluetoothRFCOMMChannel, IOBluetoothRFCOMMChannelDelegate, IOBluetoothSDPServiceRecord,
    IOBluetoothSDPUUID, IOBluetoothUserNotification, IOBluetoothUserNotificationChannelDirection,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const MAX_CONNECTIONS: usize = 8;
// Apple does not promise a write-complete callback on close. Never free a buffer
// possibly referenced by IOBluetooth: bound retired owners and refuse new sessions
// at the cap until their completions arrive instead.
const MAX_NATIVE_OWNERS: usize = 16;
const MAX_RECEIVE_CALLBACK: usize = 64 * 1024;

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static ENDPOINT: Mutex<Option<(u64, String)>> = Mutex::new(None);

thread_local! {
    static SERVER: RefCell<Option<Server>> = const { RefCell::new(None) };
    static RETIRED: RefCell<HashMap<usize, NativeConnection>> = RefCell::new(HashMap::new());
}

pub fn initialize(app: tauri::AppHandle) {
    let _ = APP.set(app);
}

pub fn local_endpoint() -> Option<String> {
    ENDPOINT.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(_, endpoint)| endpoint.clone())
}

struct ListenerGuard {
    app: tauri::AppHandle,
    generation: u64,
    token: CancellationToken,
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        self.token.cancel();
        let generation = self.generation;
        // This also runs if the caller drops the listen future while changing
        // its opt-in setting. Only the generation we own may be torn down.
        let _ = self.app.run_on_main_thread(move || stop(generation));
    }
}

pub async fn listen(accepted: mpsc::Sender<LinkConnection>, token: CancellationToken) -> Result<(), String> {
    let app = APP.get().ok_or("Bluetooth main-thread dispatcher is not initialized")?.clone();
    let token = token.child_token();
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let _guard = ListenerGuard { app: app.clone(), generation, token: token.clone() };
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| "Bluetooth requires the application async runtime")?;
    let (ready, result) = oneshot::channel();
    let native_token = token.clone();
    app.run_on_main_thread(move || {
        let result = if native_token.is_cancelled() { Ok(()) } else { start(generation, accepted, runtime, native_token) };
        let _ = ready.send(result);
    }).map_err(|e| format!("Dispatch Bluetooth listener: {e}"))?;
    tokio::select! {
        _ = token.cancelled() => return Ok(()),
        started = result => started.map_err(|_| "Bluetooth initialization was interrupted")??,
    }
    token.cancelled().await;
    Ok(())
}

struct Server {
    generation: u64,
    record: Retained<IOBluetoothSDPServiceRecord>,
    notification: Retained<IOBluetoothUserNotification>,
    delegate: Retained<RfcommDelegate>,
    local_adapter: String,
    channel: u8,
    accepted: mpsc::Sender<LinkConnection>,
    runtime: tokio::runtime::Handle,
    token: CancellationToken,
    connections: HashMap<usize, NativeConnection>,
}

struct DelegateIvars {
    generation: u64,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Every native callback
    // is registered on the main run loop and the class is main-thread-only.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "ConsiliumRfcommDelegate"]
    #[ivars = DelegateIvars]
    struct RfcommDelegate;

    unsafe impl NSObjectProtocol for RfcommDelegate {}

    impl RfcommDelegate {
        #[unsafe(method(consiliumChannelOpened:channel:))]
        fn channel_opened(&self, _notification: Option<&IOBluetoothUserNotification>, channel: Option<&IOBluetoothRFCOMMChannel>) {
            if let Some(channel) = channel { accept_channel(self.ivars().generation, channel); }
        }
    }

    unsafe impl IOBluetoothRFCOMMChannelDelegate for RfcommDelegate {
        #[unsafe(method(rfcommChannelData:data:length:))]
        unsafe fn received(&self, channel: Option<&IOBluetoothRFCOMMChannel>, data: *mut c_void, length: usize) {
            if let Some(channel) = channel {
                if length > MAX_RECEIVE_CALLBACK || (length != 0 && data.is_null()) {
                    close_connection(self.ivars().generation, channel_key(channel));
                    return;
                }
                if length != 0 {
                    // SAFETY: IOBluetooth owns this callback buffer for the duration
                    // of the callback. The bounded stream copies it synchronously.
                    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
                    receive(self.ivars().generation, channel_key(channel), bytes);
                }
            }
        }

        #[unsafe(method(rfcommChannelClosed:))]
        unsafe fn closed(&self, channel: Option<&IOBluetoothRFCOMMChannel>) {
            let _keep_alive = self.retain();
            if let Some(channel) = channel { finish_connection(self.ivars().generation, channel_key(channel), None); }
        }

        #[unsafe(method(rfcommChannelOpenComplete:status:))]
        unsafe fn opened(&self, channel: Option<&IOBluetoothRFCOMMChannel>, status: i32) {
            if let Some(channel) = channel {
                let key = channel_key(channel);
                if status != 0 { close_connection(self.ivars().generation, key); return; }
                SERVER.with(|state| {
                    if let Some(server) = state.borrow_mut().as_mut().filter(|s| s.generation == self.ivars().generation) {
                        if let Some(connection) = server.connections.get_mut(&key) { connection.opened = true; }
                    }
                });
                pump(self.ivars().generation, key);
            }
        }

        #[unsafe(method(rfcommChannelWriteComplete:refcon:status:))]
        unsafe fn write_complete(&self, channel: Option<&IOBluetoothRFCOMMChannel>, refcon: *mut c_void, status: i32) {
            let _keep_alive = self.retain();
            if let Some(channel) = channel { complete_write(self.ivars().generation, channel_key(channel), refcon, status); }
        }
    }
);

impl RfcommDelegate {
    fn new(mtm: MainThreadMarker, generation: u64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { generation });
        // SAFETY: NSObject's init signature and inherited initializer are correct.
        unsafe { msg_send![super(this), init] }
    }
}

fn channel_key(channel: &IOBluetoothRFCOMMChannel) -> usize {
    std::ptr::from_ref(channel) as usize
}

fn normalized_address(address: &str) -> Option<String> {
    let compact = address.replace([':', '-'], "").to_ascii_uppercase();
    (compact.len() == 12 && compact.bytes().all(|b| b.is_ascii_hexdigit())
        && compact != "000000000000" && compact != "FFFFFFFFFFFF").then_some(compact)
}

fn service_dictionary() -> Result<Retained<NSDictionary<NSString, NSObject>>, String> {
    let bytes = uuid::Uuid::parse_str(SERVICE_UUID).map_err(|_| "Invalid Bluetooth service UUID")?;
    // SAFETY: UUID bytes remain valid for this constructor, whose result owns a
    // copy. Protocol UUIDs are the Bluetooth-assigned L2CAP and RFCOMM identifiers.
    let (service, l2cap, rfcomm) = unsafe {
        (IOBluetoothSDPUUID::uuidWithBytes_length(bytes.as_bytes().as_ptr().cast(), 16),
         IOBluetoothSDPUUID::uuid16(0x0100), IOBluetoothSDPUUID::uuid16(0x0003))
    };
    let service = service.ok_or("Create PeerLink Bluetooth service UUID")?;
    let l2cap = l2cap.ok_or("Create L2CAP UUID")?;
    let rfcomm = rfcomm.ok_or("Create RFCOMM UUID")?;
    let classes = NSArray::<NSObject>::from_slice(&[&service]);
    let l2cap_descriptor = NSArray::<NSObject>::from_slice(&[&l2cap]);
    // An explicit unsigned byte is required by SDP for the RFCOMM channel;
    // macOS substitutes its actually assigned channel when the preferred one is in use.
    let kind = NSNumber::new_u8(1);
    let size = NSNumber::new_u8(1);
    let preferred_channel = NSNumber::new_u8(1);
    let channel = NSDictionary::<NSString, NSObject>::from_slices(
        &[ns_string!("DataElementType"), ns_string!("DataElementSize"), ns_string!("DataElementValue")],
        &[&kind, &size, &preferred_channel],
    );
    let rfcomm_descriptor = NSArray::<NSObject>::from_slice(&[&rfcomm, &channel]);
    let protocols = NSArray::<NSObject>::from_slice(&[&l2cap_descriptor, &rfcomm_descriptor]);
    let persistent = NSNumber::new_bool(false);
    let local = NSDictionary::<NSString, NSObject>::from_slices(&[ns_string!("Persistent")], &[&persistent]);
    Ok(NSDictionary::from_slices(
        &[ns_string!("0001 - ServiceClassIDList"), ns_string!("0004 - ProtocolDescriptorList"),
          ns_string!("0100 - ServiceName"), ns_string!("LocalAttributes")],
        &[&classes, &protocols, ns_string!("Consilium PeerLink"), &local],
    ))
}

fn start(generation: u64, accepted: mpsc::Sender<LinkConnection>, runtime: tokio::runtime::Handle, token: CancellationToken) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("Bluetooth listener requires the main thread")?;
    if SERVER.with(|state| state.borrow().is_some()) { return Err("Bluetooth listener is already active".into()); }
    if RETIRED.with(|state| state.borrow().len() >= MAX_NATIVE_OWNERS) {
        return Err("Bluetooth is waiting for previous native writes to finish".into());
    }
    // SAFETY: Every IOBluetooth object is created and used on the main run loop.
    let controller = unsafe { IOBluetoothHostController::defaultController() }.ok_or("No Bluetooth adapter is available")?;
    if unsafe { controller.powerState() } != BluetoothHCIPowerState::ON {
        return Err("Turn on the Mac Bluetooth radio before enabling Bluetooth in Consilium".into());
    }
    let address = unsafe { controller.addressAsString() }.ok_or("Bluetooth adapter address is unavailable")?;
    let local_adapter = normalized_address(&address.to_string()).ok_or("Bluetooth adapter address is invalid")?;
    let dictionary = service_dictionary()?;
    // SAFETY: Erasing the dictionary's generic parameters is valid: all keys and
    // values are Objective-C objects and its SDP shape was constructed above.
    let record = unsafe { IOBluetoothSDPServiceRecord::publishedServiceRecordWithDictionary(Some(dictionary.cast_unchecked())) }
        .ok_or("Publish PeerLink Bluetooth service (check Bluetooth permission in System Settings)")?;
    let mut channel = 0;
    if unsafe { record.getRFCOMMChannelID(&mut channel) } != 0 || !(1..=30).contains(&channel) {
        unsafe { record.removeServiceRecord(); }
        return Err("Bluetooth service did not receive a valid RFCOMM channel".into());
    }
    let delegate = RfcommDelegate::new(mtm, generation);
    let notification = unsafe {
        IOBluetoothRFCOMMChannel::registerForChannelOpenNotifications_selector_withChannelID_direction(
            Some(&delegate), Some(sel!(consiliumChannelOpened:channel:)), channel,
            IOBluetoothUserNotificationChannelDirection::Incoming,
        )
    };
    let Some(notification) = notification else {
        unsafe { record.removeServiceRecord(); }
        return Err("Register Bluetooth incoming channel notifications".into());
    };
    SERVER.with(|state| *state.borrow_mut() = Some(Server {
        generation, record, notification, delegate, local_adapter: local_adapter.clone(),
        channel, accepted, runtime, token, connections: HashMap::new(),
    }));
    *ENDPOINT.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, format!("rfcomm://{local_adapter}")));
    Ok(())
}

fn stop(generation: u64) {
    let server = SERVER.with(|state| {
        let mut state = state.borrow_mut();
        if state.as_ref().is_some_and(|s| s.generation == generation) { state.take() } else { None }
    });
    let Some(server) = server else { return };
    let mut endpoint = ENDPOINT.lock().unwrap_or_else(|e| e.into_inner());
    if endpoint.as_ref().is_some_and(|(id, _)| *id == generation) { *endpoint = None; }
    drop(endpoint);
    server.token.cancel();
    // Do not hold a RefCell borrow across calls into IOBluetooth: close and write
    // callbacks may reenter the delegate synchronously.
    unsafe { server.notification.unregister(); server.record.removeServiceRecord(); }
    for (_, connection) in server.connections { retire_or_close(connection, Some("Bluetooth listener stopped".into())); }
}

struct PendingWrite {
    bytes: Vec<u8>,
}

struct NativeConnection {
    generation: u64,
    channel: Retained<IOBluetoothRFCOMMChannel>,
    // Channel delegates are not retained by IOBluetooth on modern macOS.
    _delegate: Retained<RfcommDelegate>,
    bytes: NativePump,
    mtu: usize,
    opened: bool,
    pending: Option<PendingWrite>,
}

fn accept_channel(generation: u64, channel: &IOBluetoothRFCOMMChannel) {
    let key = channel_key(channel);
    let retired = RETIRED.with(|state| state.borrow().len());
    let source = SERVER.with(|state| {
        let state = state.borrow();
        state.as_ref().filter(|s| s.generation == generation && !s.token.is_cancelled()
            && s.connections.len() < MAX_CONNECTIONS && s.connections.len() + retired < MAX_NATIVE_OWNERS
            && !s.connections.contains_key(&key))
            .map(|s| (s.delegate.clone(), s.accepted.clone(), s.local_adapter.clone(), s.channel, s.runtime.clone()))
    });
    let Some((delegate, accepted, local_adapter, assigned_channel, runtime)) = source else {
        unsafe { channel.closeChannel(); }
        return;
    };
    // This listener must never accept an unrelated application's channel, nor
    // initiate pairing with an unknown device. PeerLink still checks its own key.
    let device = unsafe { channel.getDevice() };
    let remote = device.as_ref().filter(|d| unsafe { d.isPaired() })
        .and_then(|d| unsafe { d.addressString() }).and_then(|address| normalized_address(&address.to_string()));
    let mtu = usize::from(unsafe { channel.getMTU() });
    if remote.is_none() || mtu == 0 || !unsafe { channel.isIncoming() }
        || unsafe { channel.getChannelID() } != assigned_channel {
        unsafe { channel.closeChannel(); }
        return;
    }
    let app = match APP.get() { Some(app) => app.clone(), None => { unsafe { channel.closeChannel(); } return; } };
    let wake = Arc::new(move || defer_pump(&runtime, app.clone(), generation, key));
    let (stream, bytes) = byte_channel(wake);
    let connection = NativeConnection {
        generation, channel: channel.retain(), _delegate: delegate.clone(), bytes,
        mtu: mtu.min(usize::from(u16::MAX)), opened: unsafe { channel.isOpen() }, pending: None,
    };
    SERVER.with(|state| {
        if let Some(server) = state.borrow_mut().as_mut().filter(|s| s.generation == generation) {
            server.connections.insert(key, connection);
        }
    });
    if unsafe { channel.setDelegate(Some(&delegate)) } != 0 {
        close_connection(generation, key);
        return;
    }
    let evidence = RouteEvidence::BluetoothRfcomm {
        local_adapter, remote_device: remote.expect("validated above"),
        service_uuid: SERVICE_UUID.to_owned(), channel: assigned_channel,
    };
    if accepted.try_send(LinkConnection { stream: Box::new(stream), evidence }).is_err() {
        close_connection(generation, key);
        return;
    }
    pump(generation, key);
}

fn receive(generation: u64, key: usize, data: &[u8]) {
    let bytes = SERVER.with(|state| state.borrow().as_ref().filter(|s| s.generation == generation)
        .and_then(|s| s.connections.get(&key)).map(|c| c.bytes.clone()));
    if bytes.is_some_and(|bytes| data.len() > bytes.read_capacity() || bytes.incoming(data).is_err()) {
        close_connection(generation, key);
    }
}

fn defer_pump(runtime: &tokio::runtime::Handle, app: tauri::AppHandle, generation: u64, key: usize) {
    runtime.spawn(async move {
        // Tauri may execute run_on_main_thread inline when called on that
        // thread. Hop through its Tokio worker so a synchronous write-complete
        // callback cannot recursively drain a full queue through native calls.
        tokio::task::yield_now().await;
        let _ = app.run_on_main_thread(move || pump(generation, key));
    });
}

fn pump(generation: u64, key: usize) {
    let state = SERVER.with(|state| {
        state.borrow().as_ref().filter(|s| s.generation == generation).and_then(|s| s.connections.get(&key))
            .map(|c| (c.channel.clone(), c.bytes.clone(), c.mtu, c.pending.is_some(), c.opened))
    });
    let Some((channel, bytes, mtu, pending, opened)) = state else { return };
    if bytes.is_closed() { close_connection(generation, key); return; }
    if pending { return; }
    if bytes.shutdown_requested() && bytes.shutdown_ready() {
        bytes.complete_shutdown();
        // RFCOMM has no TCP-style half-close. Flush first, then close both
        // directions, leaving buffered incoming bytes readable before EOF.
        finish_connection(generation, key, None);
        return;
    }
    if !opened { return; }
    let Some(mut outgoing) = bytes.outgoing(mtu) else { return };
    if outgoing.is_empty() { return; }
    let pointer = outgoing.as_mut_ptr();
    let length = outgoing.len() as u16; // bounded by the native MTU and u16::MAX
    // Store ownership before calling native code: a completion can reenter.
    let stored = SERVER.with(|state| {
        let mut state = state.borrow_mut();
        let Some(connection) = state.as_mut().filter(|s| s.generation == generation)
            .and_then(|s| s.connections.get_mut(&key)) else { return false };
        if connection.pending.is_some() { return false; }
        connection.pending = Some(PendingWrite { bytes: outgoing });
        true
    });
    if !stored { return; }
    // SAFETY: The main-thread connection owns this nonempty, stable allocation
    // until the exact write-complete callback. No queue bytes are consumed yet.
    let status = unsafe { channel.writeAsync_length_refcon(pointer.cast(), length, pointer.cast()) };
    if status != 0 { complete_write(generation, key, pointer.cast(), status); }
}

fn complete_write(generation: u64, key: usize, refcon: *mut c_void, status: i32) {
    let completed = SERVER.with(|state| {
        let mut state = state.borrow_mut();
        let connection = state.as_mut().filter(|s| s.generation == generation)?.connections.get_mut(&key)?;
        let pending = connection.pending.as_ref()?;
        if pending.bytes.as_ptr().cast::<c_void>() != refcon.cast_const() { return None; }
        let pending = connection.pending.take()?;
        Some((connection.bytes.clone(), pending))
    });
    if let Some((bytes, pending)) = completed {
        if status != 0 || bytes.consume_written(pending.bytes.len()).is_err() {
            close_connection(generation, key);
        } else {
            let runtime = SERVER.with(|state| state.borrow().as_ref().filter(|s| s.generation == generation).map(|s| s.runtime.clone()));
            if let (Some(runtime), Some(app)) = (runtime, APP.get()) { defer_pump(&runtime, app.clone(), generation, key); }
        }
        return;
    }
    // A disabled/dropped listener may already have closed the logical stream.
    // Keep native-owned memory alive until this matching completion, independent
    // of whether a new generation has subsequently started listening.
    let retired = RETIRED.with(|state| {
        let mut state = state.borrow_mut();
        let connection = state.get(&key)?;
        if connection.generation != generation || connection.pending.as_ref()?.bytes.as_ptr().cast::<c_void>() != refcon.cast_const() {
            return None;
        }
        state.remove(&key)
    });
    if let Some(connection) = retired { unsafe { connection.channel.setDelegate(None); } }
}

fn close_connection(generation: u64, key: usize) {
    finish_connection(generation, key, Some("Bluetooth channel closed".into()));
}

fn finish_connection(generation: u64, key: usize, error: Option<String>) {
    let connection = SERVER.with(|state| {
        state.borrow_mut().as_mut().filter(|s| s.generation == generation).and_then(|s| s.connections.remove(&key))
    });
    if let Some(connection) = connection { retire_or_close(connection, error); }
}

fn retire_or_close(connection: NativeConnection, error: Option<String>) {
    let key = channel_key(&connection.channel);
    let channel = connection.channel.clone();
    connection.bytes.end_read();
    // finish(None) still reports an error when any native write is unconfirmed.
    connection.bytes.finish(error);
    if connection.pending.is_some() {
        // Closing a native channel is not proof that it stopped borrowing the
        // write allocation. A late callback owns the only safe release point.
        RETIRED.with(|state| { state.borrow_mut().insert(key, connection); });
    } else {
        unsafe { channel.setDelegate(None); }
    }
    unsafe { channel.closeChannel(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_adapter_addresses_can_be_advertised() {
        assert_eq!(normalized_address("12:34:56:ab:cd:ef"), Some("123456ABCDEF".into()));
        assert_eq!(normalized_address("12-34-56-AB-CD-EF"), Some("123456ABCDEF".into()));
        for invalid in ["", "000000000000", "FFFFFFFFFFFF", "12:34:56:GG:CD:EF", "123456789ABCDE", "rfcomm://123456ABCDEF"] {
            assert_eq!(normalized_address(invalid), None);
        }
    }
}
