//! In-memory lifetime policy. Native APIs and saved configuration stay outside
//! this module so opt-in races and cancellation can be tested without radios.
use std::future::Future;
use std::sync::{Mutex, MutexGuard};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

struct Generation {
    id: u64,
    initialized: bool,
    enabled: bool,
    token: CancellationToken,
    failure: Option<String>,
}

pub(super) struct ActiveEpoch {
    pub(super) id: u64,
    pub(super) token: CancellationToken,
}

pub(super) struct Controller {
    state: Mutex<Generation>,
    changes: watch::Sender<u64>,
}

impl Controller {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(Generation { id: 0, initialized: false, enabled: false, token: CancellationToken::new(), failure: None }),
            changes: watch::channel(0).0,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Generation> { self.state.lock().unwrap_or_else(|e| e.into_inner()) }

    pub(super) fn initialize(&self, enabled: bool) {
        let mut state = self.lock();
        // An already-applied user choice must win over a delayed/repeated load.
        if !state.initialized { self.commit(&mut state, enabled); }
    }

    pub(super) fn snapshot(&self) -> (bool, Option<String>) {
        let state = self.lock();
        (state.enabled, state.failure.clone())
    }

    pub(super) fn is_enabled(&self) -> bool { self.lock().enabled }

    pub(super) fn active_epoch(&self) -> Option<ActiveEpoch> {
        let state = self.lock();
        // The opt-in check and child acquisition share the transition lock.
        state.enabled.then(|| ActiveEpoch { id: state.id, token: state.token.child_token() })
    }

    pub(super) fn set_enabled(&self, enabled: bool, persist: impl FnOnce() -> Result<(), String>) -> Result<u64, String> {
        let mut state = self.lock();
        // Serialize the write with the transition: two commands cannot leave
        // the file disabled while a later-finishing enable runs in memory.
        persist()?;
        Ok(self.commit(&mut state, enabled))
    }

    fn commit(&self, state: &mut Generation, enabled: bool) -> u64 {
        state.token.cancel();
        state.token = CancellationToken::new();
        state.id = state.id.wrapping_add(1);
        state.initialized = true;
        state.enabled = enabled;
        state.failure = None;
        // A generation revision also wakes listeners if false->true changes
        // coalesce or the user explicitly retries the same enabled value.
        self.changes.send_replace(state.id);
        state.id
    }

    pub(super) fn record_failure(&self, generation: u64, error: String) -> bool {
        let mut state = self.lock();
        if state.id != generation || !state.enabled || state.token.is_cancelled() { return false; }
        state.failure = Some(error);
        true
    }
}

/// Own only the current listener child. Dropping this future cancels native
/// startup and accepted sessions through that child, without cancelling peers
/// using independent connection children from the same enabled generation.
pub(super) async fn run_listener<Serve, Serving>(controller: &Controller, caller: CancellationToken, mut serve: Serve) -> Result<(), String>
where
    Serve: FnMut(CancellationToken) -> Serving,
    Serving: Future<Output = Result<(), String>>,
{
    let mut changes = controller.changes.subscribe();
    loop {
        if caller.is_cancelled() { return Ok(()); }
        // Copy immediately; never hold watch's read guard while locking state.
        let observed = *changes.borrow_and_update();
        let Some(active) = controller.active_epoch() else {
            tokio::select! { biased; _ = caller.cancelled() => return Ok(()), _ = changes.changed() => continue }
        };
        if active.id != observed { continue; }
        let stop = active.token;
        let guard = stop.clone().drop_guard();
        let result = tokio::select! {
            biased;
            _ = caller.cancelled() => return Ok(()),
            _ = stop.cancelled() => continue,
            _ = changes.changed() => continue,
            result = serve(stop.clone()) => result,
        };
        stop.cancel();
        drop(guard);
        if let Err(error) = result { controller.record_failure(active.id, error); }
        // Permission/startup failures wait for an explicit retry, not a hot loop.
        tokio::select! { biased; _ = caller.cancelled() => return Ok(()), _ = changes.changed() => {} }
    }
}

/// Owns the caller->connection relay even while a connect future is pending.
/// Dropping it cancels only its child and aborts the otherwise detached task.
#[cfg(any(windows, test))]
pub(super) struct SessionRelay {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

#[cfg(any(windows, test))]
impl SessionRelay {
    pub(super) fn new(caller: CancellationToken, stop: CancellationToken) -> Self {
        if caller.is_cancelled() { stop.cancel(); }
        let child = stop.clone();
        let task = tokio::spawn(async move {
            tokio::select! { biased; _ = caller.cancelled() => child.cancel(), _ = child.cancelled() => {} }
        });
        Self { stop, task }
    }
}

#[cfg(any(windows, test))]
impl Drop for SessionRelay {
    fn drop(&mut self) { self.stop.cancel(); self.task.abort(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn set(controller: &Controller, enabled: bool) -> u64 { controller.set_enabled(enabled, || Ok(())).unwrap() }

    async fn receive(receiver: &mut mpsc::UnboundedReceiver<CancellationToken>) -> CancellationToken {
        tokio::time::timeout(Duration::from_secs(2), receiver.recv()).await.unwrap().unwrap()
    }

    #[test]
    fn disable_between_the_ui_check_and_acquisition_cannot_start_a_new_child() {
        let controller = Controller::new();
        set(&controller, true);
        assert!(controller.snapshot().0);
        let old = controller.active_epoch().unwrap();
        set(&controller, false);
        assert!(old.token.is_cancelled());
        assert!(controller.active_epoch().is_none());
        set(&controller, true);
        let fresh = controller.active_epoch().unwrap();
        assert_ne!(old.id, fresh.id);
        assert!(!fresh.token.is_cancelled());
    }

    #[test]
    fn old_listener_errors_cannot_poison_a_reenabled_generation() {
        let controller = Controller::new();
        let old = set(&controller, true);
        assert!(controller.record_failure(old, "old permission failure".into()));
        set(&controller, false);
        let fresh = set(&controller, true);
        assert!(!controller.record_failure(old, "late old failure".into()));
        assert_eq!(controller.snapshot(), (true, None));
        assert!(controller.record_failure(fresh, "current failure".into()));
        assert_eq!(controller.snapshot().1.as_deref(), Some("current failure"));
    }

    #[test]
    fn failed_persistence_keeps_the_previous_generation_and_sessions() {
        let controller = Controller::new();
        set(&controller, true);
        let old = controller.active_epoch().unwrap();
        let mut changes = controller.changes.subscribe();
        changes.borrow_and_update();
        assert_eq!(controller.set_enabled(false, || Err("saved setting unavailable".into())), Err("saved setting unavailable".into()));
        assert_eq!(controller.active_epoch().unwrap().id, old.id);
        assert!(!old.token.is_cancelled());
        assert!(!changes.has_changed().unwrap());
    }

    #[test]
    fn a_delayed_or_repeated_initial_load_cannot_overwrite_a_user_choice() {
        let controller = Controller::new();
        set(&controller, true);
        let chosen = controller.active_epoch().unwrap();
        controller.initialize(false);
        controller.initialize(false);
        assert!(controller.snapshot().0);
        assert_eq!(controller.active_epoch().unwrap().id, chosen.id);
        assert!(!chosen.token.is_cancelled());
    }

    #[test]
    fn concurrent_setting_writes_and_commits_have_the_same_order() {
        let controller = Arc::new(Controller::new());
        let sequence = Arc::new(AtomicU64::new(0));
        let start = Arc::new(Barrier::new(4));
        let workers: Vec<_> = (0..4).map(|worker| {
            let controller = controller.clone();
            let sequence = sequence.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                for round in 0..100 {
                    let mut saved_revision = 0;
                    let revision = controller.set_enabled((worker + round) % 2 == 0, || {
                        saved_revision = sequence.fetch_add(1, Ordering::SeqCst) + 1;
                        // Encourage the old write-before-lock interleaving.
                        std::thread::yield_now();
                        Ok(())
                    }).unwrap();
                    assert_eq!(revision, saved_revision);
                }
            })
        }).collect();
        for worker in workers { worker.join().unwrap(); }
        assert_eq!(sequence.load(Ordering::SeqCst), 400);
    }

    #[tokio::test]
    async fn a_fast_disable_reenable_restarts_the_listener_in_a_fresh_generation() {
        let controller = Arc::new(Controller::new());
        set(&controller, true);
        let caller = CancellationToken::new();
        let (started, mut starts) = mpsc::unbounded_channel();
        let owning_controller = controller.clone();
        let owning_caller = caller.clone();
        let listener = tokio::spawn(async move {
            run_listener(&owning_controller, owning_caller, move |stop| {
                let started = started.clone();
                async move { started.send(stop.clone()).unwrap(); stop.cancelled().await; Ok(()) }
            }).await
        });
        let old = receive(&mut starts).await;
        set(&controller, false);
        set(&controller, true);
        let fresh = receive(&mut starts).await;
        assert!(old.is_cancelled());
        assert!(!fresh.is_cancelled());
        caller.cancel();
        tokio::time::timeout(Duration::from_secs(2), listener).await.unwrap().unwrap().unwrap();
        assert!(fresh.is_cancelled());
        assert!(controller.snapshot().0, "Stopping this listener must not change the saved opt-in");
    }

    #[tokio::test]
    async fn listener_failure_waits_for_an_explicit_retry_of_the_same_enabled_value() {
        let controller = Arc::new(Controller::new());
        set(&controller, true);
        let caller = CancellationToken::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let (started, mut starts) = mpsc::unbounded_channel();
        let owning_controller = controller.clone();
        let owning_caller = caller.clone();
        let observed = attempts.clone();
        let listener = tokio::spawn(async move {
            run_listener(&owning_controller, owning_caller, move |stop| {
                let started = started.clone();
                let observed = observed.clone();
                async move {
                    if observed.fetch_add(1, Ordering::SeqCst) == 0 { return Err("fixture permission failure".into()); }
                    started.send(stop.clone()).unwrap();
                    stop.cancelled().await;
                    Ok(())
                }
            }).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while controller.snapshot().1.is_none() { tokio::task::yield_now().await; }
        }).await.unwrap();
        for _ in 0..16 { tokio::task::yield_now().await; }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        set(&controller, true);
        let retry = receive(&mut starts).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(controller.snapshot(), (true, None));
        caller.cancel();
        listener.await.unwrap().unwrap();
        assert!(retry.is_cancelled());
    }

    #[tokio::test]
    async fn dropping_the_listener_cancels_its_child_without_cancelling_other_sessions() {
        let controller = Arc::new(Controller::new());
        set(&controller, true);
        let other_session = controller.active_epoch().unwrap().token;
        let caller = CancellationToken::new();
        let (started, mut starts) = mpsc::unbounded_channel();
        let owning_controller = controller.clone();
        let owning_caller = caller.clone();
        let listener = tokio::spawn(async move {
            run_listener(&owning_controller, owning_caller, move |stop| {
                let started = started.clone();
                async move { started.send(stop.clone()).unwrap(); stop.cancelled().await; Ok(()) }
            }).await
        });
        let owned = receive(&mut starts).await;
        listener.abort();
        assert!(listener.await.unwrap_err().is_cancelled());
        assert!(owned.is_cancelled());
        assert!(!other_session.is_cancelled());
        assert!(!caller.is_cancelled());
    }

    #[tokio::test]
    async fn caller_cancellation_stops_only_its_own_connection_relay() {
        let controller = Controller::new();
        set(&controller, true);
        let stop = controller.active_epoch().unwrap().token;
        let sibling = controller.active_epoch().unwrap().token;
        let caller = CancellationToken::new();
        let mut relay = SessionRelay::new(caller.clone(), stop.clone());
        caller.cancel();
        tokio::time::timeout(Duration::from_secs(2), stop.cancelled()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), &mut relay.task).await.unwrap().unwrap();
        assert!(!sibling.is_cancelled());
    }

    #[tokio::test]
    async fn dropping_a_pending_connection_relay_cancels_the_child_and_aborts_the_task() {
        let caller = CancellationToken::new();
        let stop = CancellationToken::new();
        let relay = SessionRelay::new(caller.clone(), stop.clone());
        let task = relay.task.abort_handle();
        drop(relay);
        assert!(stop.is_cancelled());
        assert!(!caller.is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), async {
            while !task.is_finished() { tokio::task::yield_now().await; }
        }).await.unwrap();
    }

    #[tokio::test]
    async fn an_already_cancelled_caller_never_leaves_an_uncancelled_native_child() {
        let caller = CancellationToken::new();
        caller.cancel();
        let stop = CancellationToken::new();
        let _relay = SessionRelay::new(caller, stop.clone());
        assert!(stop.is_cancelled());
    }
}
