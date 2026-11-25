//! Parent owns the worker process, its private pipes, deadline, and opt-in epoch.
use super::{
    read_frame, write_frame, PairingStatus, PairingTarget, Snapshot, WorkerCommand, WorkerEvent,
};
use super::super::worker_process::OwnedChild;
use std::io::BufReader;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, SyncSender},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

static CURRENT: OnceLock<Mutex<Option<Arc<Attempt>>>> = OnceLock::new();
struct Attempt {
    snapshot: Mutex<Snapshot>,
    valid: Arc<dyn Fn() -> bool + Send + Sync>,
    epoch: CancellationToken,
    cancelled: AtomicBool,
    finished: AtomicBool,
    commands: SyncSender<WorkerCommand>,
}

impl Attempt {
    fn valid(&self) -> bool {
        !self.cancelled.load(Ordering::Acquire) && !self.epoch.is_cancelled() && (self.valid)()
    }
    fn snapshot(&self) -> std::sync::MutexGuard<'_, Snapshot> {
        self.snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    fn end(&self, state: &str, detail: &str) {
        let mut snapshot = self.snapshot();
        if !snapshot.terminal() {
            snapshot.end(state, detail);
        }
        self.cancelled.store(true, Ordering::Release);
    }
    fn refresh(&self) {
        if !self.valid() {
            self.end("cancelled", "The trusted peer session or native Bluetooth setting changed. Start a fresh attempt.");
        } else if Instant::now() >= self.snapshot().current_deadline() {
            self.end(
                "expired",
                "Bluetooth pairing expired. Cancel any old Mac prompt before trying again.",
            );
        }
    }
}

fn lookup(id: &str) -> Result<Arc<Attempt>, String> {
    let current = CURRENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let attempt = current
        .as_ref()
        .ok_or("No Bluetooth pairing attempt is active.")?;
    if attempt.snapshot().status.attempt_id != id {
        return Err("This Bluetooth pairing attempt is no longer current.".into());
    }
    Ok(attempt.clone())
}

pub(super) fn start(target: PairingTarget) -> Result<PairingStatus, String> {
    if super::super::device_address(&format!("rfcomm://{}", target.address)).is_none()
        || !(target.valid)()
    {
        return Err("Select the current Bluetooth address of a trusted connected Mac.".into());
    }
    let epoch = super::super::control()
        .active_epoch()
        .ok_or("Enable native Bluetooth before pairing.")?
        .token;
    let mut current = CURRENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if current
        .as_ref()
        .is_some_and(|attempt| !attempt.finished.load(Ordering::Acquire))
    {
        return Err("The previous Bluetooth attempt is still active or cleaning up. Cancel it before starting another.".into());
    }
    let (sender, receiver) = mpsc::sync_channel(4);
    let attempt = Arc::new(Attempt {
        snapshot: Mutex::new(Snapshot::new(&target, Instant::now())),
        valid: target.valid,
        epoch,
        cancelled: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        commands: sender,
    });
    let result = attempt.snapshot().view(Instant::now());
    let running = attempt.clone();
    std::thread::Builder::new().name("consilium-bt-pair-owner".into()).spawn(move || {
        struct Finish(Arc<Attempt>);
        impl Drop for Finish {
            fn drop(&mut self) {
                if !self.0.snapshot().terminal() { self.0.end("failed", "The attended Bluetooth worker stopped before pairing completed."); }
                self.0.finished.store(true, Ordering::Release);
            }
        }
        let _finish = Finish(running.clone());
        if run(&running, target.address, receiver).is_err() {
            running.end("failed", "The private Bluetooth worker could not complete. No pairing approval was inferred.");
        }
    }).map_err(|_| "Could not start the attended Bluetooth supervisor.")?;
    *current = Some(attempt);
    Ok(result)
}

pub(super) fn status(id: &str) -> Result<PairingStatus, String> {
    let attempt = lookup(id)?;
    attempt.refresh();
    let result = attempt.snapshot().view(Instant::now());
    Ok(result)
}

pub(super) fn confirm(id: &str, challenge_id: &str, code: &str) -> Result<PairingStatus, String> {
    let attempt = lookup(id)?;
    attempt.refresh();
    if !attempt.valid() {
        return Err("The trusted Bluetooth pairing session is no longer current.".into());
    }
    let mut snapshot = attempt.snapshot();
    let command = snapshot.confirm(challenge_id, code, Instant::now())?;
    if attempt.commands.try_send(command).is_err() {
        snapshot.end(
            "failed",
            "The private Bluetooth worker stopped before confirmation.",
        );
        attempt.cancelled.store(true, Ordering::Release);
    }
    Ok(snapshot.view(Instant::now()))
}

pub(super) fn cancel(id: &str) -> Result<PairingStatus, String> {
    let attempt = lookup(id)?;
    attempt.end("cancelled", "Bluetooth pairing cancelled.");
    let result = attempt.snapshot().view(Instant::now());
    Ok(result)
}

fn run(
    attempt: &Arc<Attempt>,
    address: String,
    commands: mpsc::Receiver<WorkerCommand>,
) -> Result<(), ()> {
    let mut owner = OwnedChild::spawn("--bluetooth-pairing-worker").map_err(|_| ())?;
    let input = owner.child.stdin.take().ok_or(())?;
    let output = owner.child.stdout.take().ok_or(())?;
    let stop = Arc::new(AtomicBool::new(false));
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _stop = Stop(stop.clone());
    let failed = Arc::new(AtomicBool::new(false));
    let (outgoing, writes) = mpsc::sync_channel(4);
    let write_stop = stop.clone();
    let write_failed = failed.clone();
    let writer = std::thread::Builder::new()
        .name("consilium-bt-pair-input".into())
        .spawn(move || {
            let mut input = input;
            while !write_stop.load(Ordering::Acquire) {
                match writes.recv_timeout(Duration::from_millis(50)) {
                    Ok(frame) => {
                        if write_frame(&mut input, &frame).is_err() {
                            write_failed.store(true, Ordering::Release);
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        })
        .map_err(|_| ())?;
    let (incoming, events) = mpsc::sync_channel(16);
    let read_failed = failed.clone();
    let reader = std::thread::Builder::new()
        .name("consilium-bt-pair-output".into())
        .spawn(move || {
            let mut output = BufReader::new(output);
            for _ in 0..32 {
                match read_frame::<WorkerEvent>(&mut output) {
                    Ok(Some(event)) => {
                        if incoming.try_send(event).is_err() {
                            break;
                        }
                    }
                    Ok(None) => return,
                    Err(_) => break,
                }
            }
            read_failed.store(true, Ordering::Release);
        })
        .map_err(|_| ())?;
    outgoing
        .try_send(WorkerCommand::Start { address })
        .map_err(|_| ())?;
    let mut exited_until = None;
    let result = (|| {
        loop {
            attempt.refresh();
            if attempt.snapshot().terminal() {
                break;
            }
            if failed.load(Ordering::Acquire) {
                return Err(());
            }
            while let Ok(command) = commands.try_recv() {
                if !attempt.valid() {
                    attempt.refresh();
                    break;
                }
                outgoing.try_send(command).map_err(|_| ())?;
            }
            while let Ok(event) = events.try_recv() {
                if !attempt.valid() {
                    attempt.refresh();
                    break;
                }
                attempt
                    .snapshot()
                    .apply(event, Instant::now())
                    .map_err(|_| ())?;
            }
            if attempt.snapshot().terminal() {
                break;
            }
            // The child may exit just before its pipe-reader thread delivers
            // the final verified flags. Drain that already-written frame first.
            if owner.child.try_wait().map_err(|_| ())?.is_some() && exited_until.is_none() {
                exited_until = Some(Instant::now() + Duration::from_millis(250));
            }
            if exited_until.is_some_and(|until| Instant::now() >= until) {
                return Err(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    })();
    // Kill only our child; its pipes then unblock every owned reader/writer.
    let _ = owner.child.kill();
    let _ = owner.child.wait();
    stop.store(true, Ordering::Release);
    drop(outgoing);
    let _ = writer.join();
    let _ = reader.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::super::worker_process::Job;
    use std::io::{BufRead, Write};
    use std::os::windows::{io::AsRawHandle, process::CommandExt};
    use std::process::{Command, Stdio};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    fn attempt(valid: Arc<AtomicBool>, epoch: CancellationToken) -> Attempt {
        let (commands, _receiver) = mpsc::sync_channel(4);
        let target = PairingTarget {
            node_id: "saved-peer".into(),
            peer_name: "Computer".into(),
            address: "A1B2C3D4E5F6".into(),
            valid: Arc::new(move || valid.load(Ordering::Acquire)),
        };
        Attempt {
            snapshot: Mutex::new(Snapshot::new(&target, Instant::now())),
            valid: target.valid,
            epoch,
            cancelled: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            commands,
        }
    }

    #[test]
    fn peer_binding_loss_clears_code_and_cannot_revive_when_peer_returns() {
        let valid = Arc::new(AtomicBool::new(true));
        let attempt = attempt(valid.clone(), CancellationToken::new());
        attempt
            .snapshot()
            .apply(
                WorkerEvent::Challenge {
                    challenge_id: uuid::Uuid::new_v4().to_string(),
                    code: "012345".into(),
                    remaining_ms: 30_000,
                },
                Instant::now(),
            )
            .unwrap();
        valid.store(false, Ordering::Release);
        attempt.refresh();
        assert_eq!(attempt.snapshot().status.state, "cancelled");
        assert!(attempt.snapshot().status.code.is_none());
        valid.store(true, Ordering::Release);
        assert!(!attempt.valid());
    }

    #[test]
    fn disabling_epoch_permanently_cancels_old_attempt_even_after_new_epoch() {
        let old_epoch = CancellationToken::new();
        let attempt = attempt(Arc::new(AtomicBool::new(true)), old_epoch.child_token());
        old_epoch.cancel();
        let fresh_epoch = CancellationToken::new();
        attempt.refresh();
        assert!(!fresh_epoch.is_cancelled());
        assert!(!attempt.valid());
        assert_eq!(attempt.snapshot().status.state, "cancelled");
    }

    #[test]
    #[ignore = "only the owned-child test invokes this isolated sleeping fixture"]
    fn owned_child_fixture() {
        if std::env::var_os("CONSILIUM_BT_JOB_FIXTURE").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            println!("BT_JOB_FIXTURE_READY");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn closing_private_job_terminates_and_reaps_only_owned_hidden_fixture() {
        // This is a test-harness child, never the installed app or Bluetooth APIs.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bluetooth::pairing::process::tests::owned_child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("CONSILIUM_BT_JOB_FIXTURE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let job = match Job::assign(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("private test job: {error}");
            }
        };
        let output = child.stdout.take().unwrap();
        let (ready, readiness) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            for _ in 0..8 {
                let mut line = String::new();
                if output.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line.trim() == "BT_JOB_FIXTURE_READY" {
                    let _ = ready.send(());
                    break;
                }
            }
        });
        let started = readiness.recv_timeout(Duration::from_secs(5)).is_ok();
        let closed_at = Instant::now();
        drop(job);
        let waited = unsafe { WaitForSingleObject(child.as_raw_handle() as HANDLE, 2_000) };
        if waited != WAIT_OBJECT_0 {
            let _ = child.kill();
        }
        let _ = child.wait().unwrap();
        let _ = reader.join();
        assert!(started, "owned fixture did not reach its sleep barrier");
        assert_eq!(waited, WAIT_OBJECT_0);
        assert!(closed_at.elapsed() < Duration::from_secs(3));
    }
}
