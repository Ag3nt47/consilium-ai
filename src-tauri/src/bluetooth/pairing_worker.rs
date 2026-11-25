//! Hidden same-executable worker. Its parent job bounds all synchronous OS calls.
//! No device inquiry, console, account/config access, or credential persistence.
use super::pairing::{read_frame, valid_code, write_frame, WorkerCommand, WorkerEvent};
use std::ffi::c_void;
use std::io::BufReader;
use std::mem::size_of;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{self, SyncSender},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};
use windows_sys::Win32::Devices::Bluetooth::*;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, ERROR_INVALID_DATA, ERROR_NOT_AUTHENTICATED, ERROR_TIMEOUT,
    HANDLE,
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    number: u32,
    io: i32,
    requirements: i32,
}
#[derive(PartialEq, Eq)]
enum Observation {
    New,
    Duplicate,
    Invalidated,
}

fn observe(
    identity: &OnceLock<Identity>,
    invalid: &AtomicBool,
    incoming: Option<Identity>,
) -> Observation {
    let Some(incoming) = incoming.filter(|value| value.number <= 999_999) else {
        invalid.store(true, Ordering::Release);
        return Observation::Invalidated;
    };
    if invalid.load(Ordering::Acquire) {
        return Observation::Invalidated;
    }
    match identity.set(incoming) {
        Ok(()) => Observation::New,
        Err(value) if identity.get() == Some(&value) => Observation::Duplicate,
        Err(_) => {
            invalid.store(true, Ordering::Release);
            Observation::Invalidated
        }
    }
}

struct Pending {
    id: String,
    identity: Identity,
    deadline: Instant,
    claimed: bool,
}
impl Pending {
    fn claim(&mut self, id: &str, code: &str, now: Instant, invalid: bool) -> bool {
        if invalid
            || self.claimed
            || now >= self.deadline
            || id != self.id
            || !valid_code(code)
            || code != format!("{:06}", self.identity.number)
        {
            return false;
        }
        self.claimed = true;
        true
    }
}

enum NativeEvent {
    Challenge {
        id: String,
        number: u32,
        deadline: Instant,
    },
    Authentication(u32),
    Response(u32),
    Invalidated,
}

struct Context {
    target: u64,
    radio: usize,
    identity: OnceLock<Identity>,
    pending: Mutex<Option<Pending>>,
    invalid: AtomicBool,
    confirmed: AtomicBool,
    callbacks: AtomicUsize,
    events: SyncSender<NativeEvent>,
}

impl Context {
    fn send(&self, event: NativeEvent) {
        if self.events.try_send(event).is_err() {
            self.invalid.store(true, Ordering::Release);
        }
    }
    fn invalidate(&self) {
        self.invalid.store(true, Ordering::Release);
        self.send(NativeEvent::Invalidated);
    }
    fn respond(&self, id: &str, code: &str) {
        // Keep the claim gate through the native call: there is no unlocked
        // logging/queue interval between final validation and submitting it.
        // Callbacks never wait on this gate: a changed method/value invalidates
        // atomically, causing the owner to terminate this entire worker.
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(pending) = pending.as_mut() else {
            self.invalidate();
            return;
        };
        if !pending.claim(
            id,
            code,
            Instant::now(),
            self.invalid.load(Ordering::Acquire),
        ) {
            self.invalidate();
            return;
        }
        let response = BLUETOOTH_AUTHENTICATE_RESPONSE {
            bthAddressRemote: BLUETOOTH_ADDRESS {
                Anonymous: BLUETOOTH_ADDRESS_0 {
                    ullLong: self.target,
                },
            },
            authMethod: BLUETOOTH_AUTHENTICATION_METHOD_NUMERIC_COMPARISON,
            Anonymous: BLUETOOTH_AUTHENTICATE_RESPONSE_0 {
                numericCompInfo: BLUETOOTH_NUMERIC_COMPARISON_INFO {
                    NumericValue: pending.identity.number,
                },
            },
            negativeResponse: 0,
        };
        self.confirmed.store(true, Ordering::Release);
        // SAFETY: SDK structure and current exact-target numeric response; the
        // owning child process keeps its registration/context/radio alive.
        let result = if self.invalid.load(Ordering::Acquire) {
            ERROR_CANCELLED
        } else {
            unsafe { BluetoothSendAuthenticationResponseEx(self.radio as HANDLE, &response) }
        };
        self.send(NativeEvent::Response(result));
    }
}

unsafe extern "system" fn callback(
    parameter: *const c_void,
    parameters: *const BLUETOOTH_AUTHENTICATION_CALLBACK_PARAMS,
) -> i32 {
    if parameter.is_null() || parameters.is_null() {
        return 1;
    }
    // SAFETY: Arc root is held until unregistration and all callbacks complete,
    // or until this isolated process exits. Callback data is copied immediately.
    let context = unsafe { &*(parameter as *const Context) };
    context.callbacks.fetch_add(1, Ordering::AcqRel);
    struct Active<'a>(&'a AtomicUsize);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }
    let _active = Active(&context.callbacks);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let parameters = unsafe { *parameters };
        let target = unsafe { parameters.deviceInfo.Address.Anonymous.ullLong };
        if target != context.target {
            return;
        }
        let incoming = if parameters.authenticationMethod
            == BLUETOOTH_AUTHENTICATION_METHOD_NUMERIC_COMPARISON
        {
            Some(Identity {
                // SAFETY: This method selects the numeric-comparison member of the SDK union.
                number: unsafe { parameters.Anonymous.Numeric_Value },
                io: parameters.ioCapability,
                requirements: parameters.authenticationRequirements,
            })
        } else {
            None
        };
        match observe(&context.identity, &context.invalid, incoming) {
            Observation::Duplicate => {} // Never reject/approve the same pending callback twice.
            Observation::Invalidated => context.invalidate(),
            Observation::New => {
                let identity = incoming.expect("numeric identity was validated");
                let id = uuid::Uuid::new_v4().to_string();
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut pending = context
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if context.invalid.load(Ordering::Acquire) {
                    return;
                }
                *pending = Some(Pending {
                    id: id.clone(),
                    identity,
                    deadline,
                    claimed: false,
                });
                context.send(NativeEvent::Challenge {
                    id,
                    number: identity.number,
                    deadline,
                });
            }
        }
    }));
    if result.is_err() {
        context.invalidate();
    }
    1
}

fn device(target: u64) -> BLUETOOTH_DEVICE_INFO {
    BLUETOOTH_DEVICE_INFO {
        dwSize: size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
        Address: BLUETOOTH_ADDRESS {
            Anonymous: BLUETOOTH_ADDRESS_0 { ullLong: target },
        },
        ..Default::default()
    }
}

fn flags(radio: HANDLE, target: u64) -> Result<(bool, bool), u32> {
    let mut device = device(target);
    // SAFETY: exact target only; query does not initiate inquiry or authentication.
    let result = unsafe { BluetoothGetDeviceInfo(radio, &mut device) };
    if result == 0 {
        Ok((device.fAuthenticated != 0, device.fRemembered != 0))
    } else {
        Err(result)
    }
}

pub(super) fn run() -> i32 {
    // Backstop even if an OS call blocks during cleanup. Parent job/deadline is
    // primary; exiting the owned process reclaims all native threads and handles.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(100));
        std::process::exit(2);
    });
    let input_failed = Arc::new(AtomicBool::new(false));
    let failed_input = input_failed.clone();
    let (incoming, commands) = mpsc::sync_channel(4);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = BufReader::new(stdin.lock());
        for _ in 0..4 {
            match read_frame::<WorkerCommand>(&mut input) {
                Ok(Some(frame)) => {
                    if incoming.try_send(frame).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
        failed_input.store(true, Ordering::Release);
    });
    let Ok(WorkerCommand::Start { address }) = commands.recv_timeout(Duration::from_secs(5)) else {
        return 2;
    };
    let Some(normalized) = super::device_address(&format!("rfcomm://{address}")) else {
        return 2;
    };
    let Ok(target) = u64::from_str_radix(&normalized, 16) else {
        return 2;
    };
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut radio = std::ptr::null_mut();
    let find = BLUETOOTH_FIND_RADIO_PARAMS {
        dwSize: size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
    };
    // SAFETY: exact SDK output handles. No device inquiry is performed.
    let found = unsafe { BluetoothFindFirstRadio(&find, &mut radio) };
    if found.is_null() {
        let _ = write_frame(
            &mut output,
            &WorkerEvent::Failed {
                code: ERROR_NOT_AUTHENTICATED,
            },
        );
        return 1;
    }
    unsafe {
        BluetoothFindRadioClose(found);
    }
    match flags(radio, target) {
        Ok((true, true)) => {
            let _ = write_frame(
                &mut output,
                &WorkerEvent::Paired {
                    authenticated: true,
                    remembered: true,
                },
            );
            unsafe {
                CloseHandle(radio);
            }
            return 0;
        }
        Err(code) => {
            let _ = write_frame(&mut output, &WorkerEvent::Failed { code });
            unsafe {
                CloseHandle(radio);
            }
            return 1;
        }
        _ => {}
    }
    let (events, native) = mpsc::sync_channel(8);
    let context = Arc::new(Context {
        target,
        radio: radio as usize,
        identity: OnceLock::new(),
        pending: Mutex::new(None),
        invalid: AtomicBool::new(false),
        confirmed: AtomicBool::new(false),
        callbacks: AtomicUsize::new(0),
        events,
    });
    let callback_root = Arc::into_raw(context.clone());
    let mut registration = 0;
    let exact_device = device(target);
    // SAFETY: callback_root is held until callback cleanup or process exit.
    let registered = unsafe {
        BluetoothRegisterForAuthenticationEx(
            &exact_device,
            &mut registration,
            Some(callback),
            callback_root.cast(),
        )
    };
    if registered != 0 {
        unsafe {
            drop(Arc::from_raw(callback_root));
            CloseHandle(radio);
        }
        let _ = write_frame(&mut output, &WorkerEvent::Failed { code: registered });
        return 1;
    }
    let _ = write_frame(&mut output, &WorkerEvent::Ready {});
    let request_context = context.clone();
    let request = std::thread::spawn(move || {
        let mut device = device(target);
        // SAFETY: one exact trusted target; mandatory MITM general bonding.
        let result = unsafe {
            BluetoothAuthenticateDeviceEx(
                std::ptr::null_mut(),
                request_context.radio as HANDLE,
                &mut device,
                std::ptr::null(),
                MITMProtectionRequiredGeneralBonding,
            )
        };
        request_context.send(NativeEvent::Authentication(result));
    });
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut responder = None;
    let mut authentication = None;
    let mut response = None;
    let mut settling = None;
    let mut paired = false;
    let result = (|| -> Result<(), ()> {
        loop {
            if input_failed.load(Ordering::Acquire) {
                return Err(());
            }
            if context.invalid.load(Ordering::Acquire) {
                write_frame(&mut output, &WorkerEvent::Invalidated {}).map_err(|_| ())?;
                return Ok(());
            }
            while let Ok(command) = commands.try_recv() {
                let WorkerCommand::Confirm { challenge_id, code } = command else {
                    context.invalidate();
                    break;
                };
                if responder.is_some() {
                    context.invalidate();
                    break;
                }
                write_frame(&mut output, &WorkerEvent::Confirming {}).map_err(|_| ())?;
                let confirm_context = context.clone();
                responder = Some(std::thread::spawn(move || {
                    confirm_context.respond(&challenge_id, &code)
                }));
            }
            while let Ok(event) = native.try_recv() {
                match event {
                    NativeEvent::Challenge {
                        id,
                        number,
                        deadline,
                    } => {
                        let remaining_ms = deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis()
                            .min(30_000) as u64;
                        write_frame(
                            &mut output,
                            &WorkerEvent::Challenge {
                                challenge_id: id,
                                code: format!("{number:06}"),
                                remaining_ms,
                            },
                        )
                        .map_err(|_| ())?;
                    }
                    NativeEvent::Authentication(code) => {
                        authentication = Some(code);
                        settling = Some(Instant::now() + Duration::from_secs(5));
                    }
                    NativeEvent::Response(code) => response = Some(code),
                    NativeEvent::Invalidated => context.invalid.store(true, Ordering::Release),
                }
            }
            if context.invalid.load(Ordering::Acquire) {
                continue;
            }
            if context.confirmed.load(Ordering::Acquire) && flags(radio, target) == Ok((true, true))
            {
                if context.invalid.load(Ordering::Acquire) {
                    continue;
                }
                paired = true;
                write_frame(
                    &mut output,
                    &WorkerEvent::Paired {
                        authenticated: true,
                        remembered: true,
                    },
                )
                .map_err(|_| ())?;
                return Ok(());
            }
            if Instant::now() >= deadline || settling.is_some_and(|until| Instant::now() >= until) {
                let code = response
                    .filter(|code| *code != 0)
                    .or(authentication.filter(|code| *code != 0))
                    .unwrap_or(ERROR_TIMEOUT);
                write_frame(&mut output, &WorkerEvent::Failed { code }).map_err(|_| ())?;
                return Ok(());
            }
            if authentication.is_some() && !context.confirmed.load(Ordering::Acquire) {
                write_frame(
                    &mut output,
                    &WorkerEvent::Failed {
                        code: authentication.unwrap_or(ERROR_INVALID_DATA),
                    },
                )
                .map_err(|_| ())?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    })();
    context.invalid.store(true, Ordering::Release);
    // Callbacks are brief and never own native response I/O. If a native thread
    // is still blocked, retain its context/radio until the process exits.
    let unregistered = unsafe { BluetoothUnregisterAuthentication(registration) } != 0;
    let response_done = responder
        .as_ref()
        .is_none_or(std::thread::JoinHandle::is_finished);
    if unregistered
        && context.callbacks.load(Ordering::Acquire) == 0
        && request.is_finished()
        && response_done
    {
        let _ = request.join();
        if let Some(responder) = responder {
            let _ = responder.join();
        }
        unsafe {
            drop(Arc::from_raw(callback_root));
            CloseHandle(radio);
        }
    }
    if result.is_ok() && paired {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(number: u32) -> Identity {
        Identity {
            number,
            io: BLUETOOTH_IO_CAPABILITY_DISPLAYYESNO,
            requirements: BLUETOOTH_MITM_ProtectionRequiredGeneralBonding,
        }
    }
    #[test]
    fn duplicate_callback_never_rejects_current_numeric_comparison() {
        let identity_slot = OnceLock::new();
        let invalid = AtomicBool::new(false);
        assert!(observe(&identity_slot, &invalid, Some(identity(12345))) == Observation::New);
        assert!(observe(&identity_slot, &invalid, Some(identity(12345))) == Observation::Duplicate);
        assert!(!invalid.load(Ordering::Acquire));
    }
    #[test]
    fn changed_numeric_or_method_permanently_invalidates_attempt() {
        let slot = OnceLock::new();
        let invalid = AtomicBool::new(false);
        observe(&slot, &invalid, Some(identity(12345)));
        assert!(observe(&slot, &invalid, Some(identity(54321))) == Observation::Invalidated);
        assert!(observe(&slot, &invalid, Some(identity(12345))) == Observation::Invalidated);
        let slot = OnceLock::new();
        let invalid = AtomicBool::new(false);
        observe(&slot, &invalid, Some(identity(12345)));
        assert!(observe(&slot, &invalid, None) == Observation::Invalidated);
        assert!(invalid.load(Ordering::Acquire));
    }
    #[test]
    fn exact_entered_digits_generation_deadline_and_one_use_guard_native_response() {
        let now = Instant::now();
        let mut pending = Pending {
            id: "current".into(),
            identity: identity(12345),
            deadline: now + Duration::from_secs(30),
            claimed: false,
        };
        assert!(!pending.claim("stale", "012345", now, false));
        assert!(!pending.claim("current", "12345", now, false));
        assert!(!pending.claim("current", "012345", now, true));
        assert!(!pending.claim("current", "012345", now + Duration::from_secs(30), false));
        assert!(pending.claim("current", "012345", now, false));
        assert!(!pending.claim("current", "012345", now, false));
    }
    #[test]
    fn sdk_enum_and_abi_match_the_actual_callback_and_response() {
        assert_eq!(BLUETOOTH_AUTHENTICATION_METHOD_NUMERIC_COMPARISON, 3);
        assert_eq!(MITMProtectionRequiredGeneralBonding, 5);
        assert_eq!(size_of::<BLUETOOTH_AUTHENTICATION_CALLBACK_PARAMS>(), 576);
        assert_eq!(size_of::<BLUETOOTH_AUTHENTICATE_RESPONSE>(), 48);
        assert_eq!(
            std::mem::offset_of!(BLUETOOTH_AUTHENTICATE_RESPONSE, negativeResponse),
            44
        );
    }
}
