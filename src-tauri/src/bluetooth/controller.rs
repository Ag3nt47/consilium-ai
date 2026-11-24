//! In-memory lifetime policy. Native APIs and saved configuration stay outside
//! this module so opt-in races and cancellation can be tested without radios.
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

struct Generation {
    id: u64,
    initialized: bool,
    enabled: bool,
    token: CancellationToken,
    failure: Option<ListenerFailure>,
    next_listener: u64,
    next_attempt: u64,
    listener: Option<ListenerState>,
}

// Settings epochs own connections; a listener lease additionally fences Link
// stop/start replacements, and an attempt fences retries within that same lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListenerOwner { epoch: u64, lease: u64, attempt: u64 }
struct ListenerFailure { owner: ListenerOwner, error: String }
struct ListenerState {
    id: u64,
    lifetime: CancellationToken,
    active: Option<ListenerAttemptState>,
}
struct ListenerAttemptState { owner: ListenerOwner, token: CancellationToken, ready: bool }
struct ListenerAttempt { owner: ListenerOwner, token: CancellationToken }
struct ListenerLease { controller: Controller, id: u64, lifetime: CancellationToken }
impl Drop for ListenerLease {
    fn drop(&mut self) {
        self.lifetime.cancel();
        self.controller.release_listener(self.id);
    }
}

pub(super) type ListenerReady = Box<dyn FnOnce() + Send + 'static>;

#[cfg(any(windows, test))]
pub(super) struct ActiveEpoch {
    #[cfg(test)]
    pub(super) id: u64,
    pub(super) token: CancellationToken,
}

#[derive(Clone)]
pub(super) struct Controller {
    state: Arc<Mutex<Generation>>,
    changes: watch::Sender<u64>,
}

impl Controller {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(Generation { id: 0, initialized: false, enabled: false, token: CancellationToken::new(), failure: None, next_listener: 0, next_attempt: 0, listener: None })),
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
        (state.enabled, state.failure.as_ref().map(|failure| failure.error.clone()))
    }

    pub(super) fn is_enabled(&self) -> bool { self.lock().enabled }

    #[cfg(any(windows, test))]
    pub(super) fn active_epoch(&self) -> Option<ActiveEpoch> {
        let state = self.lock();
        // The opt-in check and child acquisition share the transition lock.
        state.enabled.then(|| ActiveEpoch {
            #[cfg(test)]
            id: state.id,
            token: state.token.child_token(),
        })
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
        if let Some(listener) = &mut state.listener { listener.active = None; }
        // A generation revision also wakes listeners if false->true changes
        // coalesce or the user explicitly retries the same enabled value.
        self.changes.send_replace(state.id);
        state.id
    }

    fn begin_listener(&self, caller: &CancellationToken) -> Option<ListenerLease> {
        let mut state = self.lock();
        if caller.is_cancelled() { return None; }
        // Supersession is permanent for this runner, including future settings
        // changes. Its eventual Drop may retire only its own lease.
        if let Some(old) = state.listener.take() {
            old.lifetime.cancel();
            if let Some(active) = old.active { active.token.cancel(); }
        }
        state.next_listener = state.next_listener.wrapping_add(1);
        let id = state.next_listener;
        let lifetime = caller.child_token();
        state.listener = Some(ListenerState { id, lifetime: lifetime.clone(), active: None });
        Some(ListenerLease { controller: self.clone(), id, lifetime })
    }

    fn begin_attempt(&self, lease: &ListenerLease) -> Option<ListenerAttempt> {
        let mut state = self.lock();
        if !state.enabled || lease.lifetime.is_cancelled()
            || state.listener.as_ref().is_none_or(|current| current.id != lease.id) { return None; }
        state.next_attempt = state.next_attempt.wrapping_add(1);
        let owner = ListenerOwner { epoch: state.id, lease: lease.id, attempt: state.next_attempt };
        let token = state.token.child_token();
        let listener = state.listener.as_mut().expect("Current listener was checked");
        if let Some(old) = listener.active.take() { old.token.cancel(); }
        listener.active = Some(ListenerAttemptState { owner, token: token.clone(), ready: false });
        Some(ListenerAttempt { owner, token })
    }

    fn owns(state: &Generation, owner: ListenerOwner) -> bool {
        state.enabled && state.id == owner.epoch && state.listener.as_ref().is_some_and(|listener|
            listener.id == owner.lease && !listener.lifetime.is_cancelled() && listener.active.as_ref().is_some_and(|active|
                active.owner == owner && !active.token.is_cancelled()))
    }

    fn record_ready(&self, owner: ListenerOwner) -> bool {
        let mut state = self.lock();
        // Failure is terminal for an attempt, even if a delayed startup
        // callback is delivered before the serving future drops its token.
        if !Self::owns(&state, owner)
            || state.failure.as_ref().is_some_and(|failure| failure.owner == owner) { return false; }
        state.listener.as_mut().and_then(|listener| listener.active.as_mut()).expect("Owned attempt exists").ready = true;
        // Only publication of the current native server clears an older error.
        state.failure = None;
        true
    }

    fn ready_callback(&self, owner: ListenerOwner) -> ListenerReady {
        let controller = self.clone();
        Box::new(move || { controller.record_ready(owner); })
    }

    fn record_failure(&self, owner: ListenerOwner, error: String) -> bool {
        let mut state = self.lock();
        if !Self::owns(&state, owner) { return false; }
        state.listener.as_mut().and_then(|listener| listener.active.as_mut()).expect("Owned attempt exists").ready = false;
        state.failure = Some(ListenerFailure { owner, error });
        true
    }

    fn release_listener(&self, id: u64) {
        let mut state = self.lock();
        if !state.listener.as_ref().is_some_and(|listener| listener.id == id) { return; }
        if let Some(listener) = state.listener.take() {
            listener.lifetime.cancel();
            if let Some(active) = listener.active { active.token.cancel(); }
        }
    }

    pub(super) fn listener_ready(&self) -> bool {
        let state = self.lock();
        state.listener.as_ref().and_then(|listener| listener.active.as_ref())
            .is_some_and(|active| active.ready && Self::owns(&state, active.owner)
                && state.failure.as_ref().is_none_or(|failure| failure.owner != active.owner))
    }
}

/// Own only the current listener child. Dropping this future cancels native
/// startup and accepted sessions through that child, without cancelling peers
/// using independent connection children from the same enabled generation.
pub(super) async fn run_listener<Serve, Serving>(controller: &Controller, caller: CancellationToken, mut serve: Serve) -> Result<(), String>
where
    Serve: FnMut(CancellationToken, ListenerReady) -> Serving,
    Serving: Future<Output = Result<(), String>>,
{
    let Some(lease) = controller.begin_listener(&caller) else { return Ok(()) };
    let mut changes = controller.changes.subscribe();
    loop {
        if caller.is_cancelled() || lease.lifetime.is_cancelled() { return Ok(()); }
        // Copy immediately; never hold watch's read guard while locking state.
        let observed = *changes.borrow_and_update();
        let Some(active) = controller.begin_attempt(&lease) else {
            tokio::select! { biased;
                _ = caller.cancelled() => return Ok(()),
                _ = lease.lifetime.cancelled() => return Ok(()),
                _ = changes.changed() => continue,
            }
        };
        if active.owner.epoch != observed { active.token.cancel(); continue; }
        let stop = active.token;
        let guard = stop.clone().drop_guard();
        let result = tokio::select! {
            biased;
            _ = caller.cancelled() => return Ok(()),
            _ = lease.lifetime.cancelled() => return Ok(()),
            _ = stop.cancelled() => continue,
            _ = changes.changed() => continue,
            result = serve(stop.clone(), controller.ready_callback(active.owner)) => result,
        };
        if let Err(error) = result { controller.record_failure(active.owner, error); }
        stop.cancel();
        drop(guard);
        // Permission/startup failures wait for an explicit retry, not a hot loop.
        tokio::select! { biased;
            _ = caller.cancelled() => return Ok(()),
            _ = lease.lifetime.cancelled() => return Ok(()),
            _ = changes.changed() => {},
        }
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
        set(&controller, true);
        let lease = controller.begin_listener(&CancellationToken::new()).unwrap();
        let old = controller.begin_attempt(&lease).unwrap();
        assert!(controller.record_failure(old.owner, "old permission failure".into()));
        set(&controller, false);
        set(&controller, true);
        let fresh = controller.begin_attempt(&lease).unwrap();
        assert!(old.token.is_cancelled());
        assert!(!controller.record_failure(old.owner, "late old failure".into()));
        assert_eq!(controller.snapshot(), (true, None));
        assert!(controller.record_failure(fresh.owner, "current failure".into()));
        assert_eq!(controller.snapshot().1.as_deref(), Some("current failure"));
    }

    #[test]
    fn replacement_requires_its_own_publication_before_clearing_an_old_error() {
        let controller = Controller::new();
        let epoch = set(&controller, true);
        let old_lease = controller.begin_listener(&CancellationToken::new()).unwrap();
        let old = controller.begin_attempt(&old_lease).unwrap();
        let queued_old_ready = controller.ready_callback(old.owner);
        assert!(controller.record_failure(old.owner, "previous start failed".into()));
        let replacement_lease = controller.begin_listener(&CancellationToken::new()).unwrap();
        let replacement = controller.begin_attempt(&replacement_lease).unwrap();
        assert_eq!(replacement.owner.epoch, epoch, "Replacement needs no opt-in toggle");
        assert!(old.token.is_cancelled());
        assert!(!controller.listener_ready());
        assert_eq!(controller.snapshot().1.as_deref(), Some("previous start failed"));
        queued_old_ready();
        assert!(!controller.record_failure(old.owner, "queued old failure".into()));
        assert_eq!(controller.snapshot().1.as_deref(), Some("previous start failed"));
        controller.ready_callback(replacement.owner)();
        assert!(controller.listener_ready());
        assert_eq!(controller.snapshot(), (true, None));
        assert!(controller.record_failure(replacement.owner, "current native failure".into()));
        assert!(!controller.record_ready(old.owner));
        assert!(!controller.record_ready(replacement.owner), "A failed attempt cannot later publish ready");
        assert!(!controller.listener_ready());
        assert_eq!(controller.snapshot().1.as_deref(), Some("current native failure"));
    }

    #[test]
    fn a_late_old_drop_cannot_cancel_or_unpublish_the_replacement() {
        let controller = Controller::new();
        set(&controller, true);
        let old_lease = controller.begin_listener(&CancellationToken::new()).unwrap();
        let old = controller.begin_attempt(&old_lease).unwrap();
        let new_lease = controller.begin_listener(&CancellationToken::new()).unwrap();
        let new = controller.begin_attempt(&new_lease).unwrap();
        assert!(controller.record_ready(new.owner));
        drop(old_lease);
        assert!(old.token.is_cancelled());
        assert!(!new.token.is_cancelled());
        assert!(controller.listener_ready());
        assert!(!new_lease.lifetime.is_cancelled());
        drop(new_lease);
        assert!(new.token.is_cancelled());
        assert!(!controller.listener_ready());
    }

    #[test]
    fn callbacks_are_owned_by_the_attempt_even_within_the_same_listener_lease() {
        let controller = Controller::new();
        set(&controller, true);
        let lease = controller.begin_listener(&CancellationToken::new()).unwrap();
        let old = controller.begin_attempt(&lease).unwrap();
        assert!(controller.record_failure(old.owner, "first attempt failed".into()));
        let fresh = controller.begin_attempt(&lease).unwrap();
        assert_eq!(old.owner.epoch, fresh.owner.epoch);
        assert_eq!(old.owner.lease, fresh.owner.lease);
        assert_ne!(old.owner.attempt, fresh.owner.attempt);
        assert!(old.token.is_cancelled());
        assert!(!controller.record_ready(old.owner));
        assert!(!controller.record_failure(old.owner, "delayed first error".into()));
        assert_eq!(controller.snapshot().1.as_deref(), Some("first attempt failed"));
        assert!(controller.record_ready(fresh.owner));
        assert!(controller.listener_ready());
        assert_eq!(controller.snapshot(), (true, None));
    }

    #[test]
    fn an_already_cancelled_listener_caller_cannot_displace_a_healthy_listener() {
        let controller = Controller::new();
        set(&controller, true);
        let lease = controller.begin_listener(&CancellationToken::new()).unwrap();
        let active = controller.begin_attempt(&lease).unwrap();
        assert!(controller.record_ready(active.owner));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(controller.begin_listener(&cancelled).is_none());
        assert!(!active.token.is_cancelled());
        assert!(controller.listener_ready());
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
            run_listener(&owning_controller, owning_caller, move |stop, ready| {
                let started = started.clone();
                async move { ready(); started.send(stop.clone()).unwrap(); stop.cancelled().await; Ok(()) }
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
    async fn link_stop_start_and_late_old_future_drop_do_not_cancel_the_new_listener() {
        let controller = Controller::new();
        let epoch = set(&controller, true);
        let old_caller = CancellationToken::new();
        let (old_started, mut old_starts) = mpsc::unbounded_channel();
        let mut old = Box::pin(run_listener(&controller, old_caller.clone(), move |stop, ready| {
            let started = old_started.clone();
            async move { ready(); started.send(stop.clone()).unwrap(); stop.cancelled().await; Ok(()) }
        }));
        // Poll only the original startup, then deliberately leave its future
        // unpolled until its replacement is ready. No scheduler timing needed.
        std::future::poll_fn(|cx| {
            assert!(old.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        }).await;
        let old_stop = receive(&mut old_starts).await;
        assert!(controller.listener_ready());
        old_caller.cancel();
        assert!(!controller.listener_ready(), "Caller cancellation immediately invalidates publication");
        let new_caller = CancellationToken::new();
        let (new_started, mut new_starts) = mpsc::unbounded_channel();
        let owner = controller.clone();
        let caller = new_caller.clone();
        let new = tokio::spawn(async move {
            run_listener(&owner, caller, move |stop, ready| {
                let started = new_started.clone();
                async move { ready(); started.send(stop.clone()).unwrap(); stop.cancelled().await; Ok(()) }
            }).await
        });
        let new_stop = receive(&mut new_starts).await;
        assert_eq!(controller.active_epoch().unwrap().id, epoch);
        drop(old);
        assert!(old_stop.is_cancelled());
        assert!(!new_stop.is_cancelled());
        assert!(controller.listener_ready());
        set(&controller, false);
        set(&controller, true);
        let next_stop = receive(&mut new_starts).await;
        assert!(new_stop.is_cancelled());
        assert!(!next_stop.is_cancelled());
        assert!(old_starts.try_recv().is_err(), "Retired runner must not restart on an opt-in change");
        new_caller.cancel();
        tokio::time::timeout(Duration::from_secs(2), new).await.unwrap().unwrap().unwrap();
        assert!(next_stop.is_cancelled());
        assert!(!controller.listener_ready());
    }

    #[tokio::test]
    async fn a_superseded_listener_runner_exits_instead_of_stealing_the_next_epoch() {
        let controller = Controller::new();
        set(&controller, true);
        let (old_started, mut old_starts) = mpsc::unbounded_channel();
        let old_owner = controller.clone();
        let old = tokio::spawn(async move {
            run_listener(&old_owner, CancellationToken::new(), move |stop, ready| {
                let started = old_started.clone();
                async move { ready(); started.send(stop.clone()).unwrap(); stop.cancelled().await; Ok(()) }
            }).await
        });
        let old_stop = receive(&mut old_starts).await;
        let (new_started, mut new_starts) = mpsc::unbounded_channel();
        let new_owner = controller.clone();
        let caller = CancellationToken::new();
        let new_caller = caller.clone();
        let new = tokio::spawn(async move {
            run_listener(&new_owner, new_caller, move |stop, ready| {
                let started = new_started.clone();
                async move { ready(); started.send(stop.clone()).unwrap(); stop.cancelled().await; Ok(()) }
            }).await
        });
        let current = receive(&mut new_starts).await;
        tokio::time::timeout(Duration::from_secs(2), old).await.unwrap().unwrap().unwrap();
        assert!(old_stop.is_cancelled());
        assert!(!current.is_cancelled());
        set(&controller, false);
        set(&controller, true);
        let next = receive(&mut new_starts).await;
        assert!(current.is_cancelled());
        assert!(old_starts.try_recv().is_err());
        assert!(controller.listener_ready());
        caller.cancel();
        tokio::time::timeout(Duration::from_secs(2), new).await.unwrap().unwrap().unwrap();
        assert!(next.is_cancelled());
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
            run_listener(&owning_controller, owning_caller, move |stop, ready| {
                let started = started.clone();
                let observed = observed.clone();
                async move {
                    if observed.fetch_add(1, Ordering::SeqCst) == 0 { return Err("fixture permission failure".into()); }
                    ready();
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
            run_listener(&owning_controller, owning_caller, move |stop, ready| {
                let started = started.clone();
                async move { ready(); started.send(stop.clone()).unwrap(); stop.cancelled().await; Ok(()) }
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
