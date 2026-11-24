//! Generation ordering for main-thread native resources. No radio APIs here.
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct ListenerGenerations { latest: AtomicU64 }
impl ListenerGenerations {
    pub(super) const fn new() -> Self { Self { latest: AtomicU64::new(0) } }
    pub(super) fn issue(&self) -> u64 { self.latest.fetch_add(1, Ordering::AcqRel).wrapping_add(1) }
    fn current(&self, generation: u64, cancelled: bool) -> bool {
        !cancelled && self.latest.load(Ordering::Acquire) == generation
    }
    pub(super) fn owns(active: Option<u64>, generation: u64) -> bool { active == Some(generation) }

    /// Native calls may reenter or yield to a newer request. Build remains
    /// uncommitted until the final check; dropping a superseded build cleans it.
    pub(super) fn replace<Resource, Error>(
        &self, generation: u64, cancelled: impl Fn() -> bool,
        active: impl Fn() -> Option<u64>, close: impl FnOnce(u64),
        build: impl FnOnce() -> Result<Resource, Error>, install: impl FnOnce(Resource),
    ) -> Result<bool, Error> {
        if !self.current(generation, cancelled()) { return Ok(false); }
        if let Some(previous) = active() {
            if previous >= generation { return Ok(false); }
            close(previous);
        }
        if !self.current(generation, cancelled()) || active().is_some() { return Ok(false); }
        let prepared = build()?;
        if !self.current(generation, cancelled()) || active().is_some() { return Ok(false); }
        install(prepared);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct Resource { id: u64, disposed: Rc<RefCell<Vec<u64>>> }
    impl Drop for Resource { fn drop(&mut self) { self.disposed.borrow_mut().push(self.id); } }

    struct Fixture {
        generations: ListenerGenerations,
        active: RefCell<Option<Resource>>,
        disposed: Rc<RefCell<Vec<u64>>>,
    }
    impl Fixture {
        fn new() -> Self { Self { generations: ListenerGenerations::new(), active: RefCell::new(None), disposed: Rc::new(RefCell::new(vec![])) } }
        fn owner(&self) -> Option<u64> { self.active.borrow().as_ref().map(|resource| resource.id) }
        fn stop(&self, id: u64) {
            if ListenerGenerations::owns(self.owner(), id) { self.active.borrow_mut().take(); }
        }
        fn start(&self, id: u64, cancelled: &Cell<bool>, during_build: impl FnOnce()) -> bool {
            self.generations.replace(id, || cancelled.get(), || self.owner(), |previous| self.stop(previous), || {
                during_build();
                Ok::<_, ()>(Resource { id, disposed: self.disposed.clone() })
            }, |resource| *self.active.borrow_mut() = Some(resource)).unwrap()
        }
    }

    #[test]
    fn a_new_listener_replaces_the_old_service_before_late_old_cleanup() {
        let fixture = Fixture::new();
        let cancelled = Cell::new(false);
        let first = fixture.generations.issue();
        assert!(fixture.start(first, &cancelled, || {}));
        let next = fixture.generations.issue();
        assert!(fixture.start(next, &cancelled, || {
            assert_eq!(fixture.owner(), None, "Old service must be removed before a replacement is published");
            assert_eq!(*fixture.disposed.borrow(), vec![first]);
        }));
        fixture.stop(first); // delayed old ListenerGuard on the main queue
        assert_eq!(fixture.owner(), Some(next));
        assert_eq!(*fixture.disposed.borrow(), vec![first]);
    }

    #[test]
    fn queued_older_start_cannot_replace_or_resurrect_after_a_newer_listener() {
        let fixture = Fixture::new();
        let cancelled = Cell::new(false);
        let first = fixture.generations.issue();
        let next = fixture.generations.issue();
        assert!(fixture.start(next, &cancelled, || {}));
        assert!(!fixture.start(first, &cancelled, || panic!("Stale queued startup must not call native APIs")));
        fixture.stop(next);
        assert!(!fixture.start(first, &cancelled, || panic!("A late start must not resurrect after the new owner stops")));
        assert_eq!(fixture.owner(), None);
    }

    #[test]
    fn supersession_during_publication_disposes_the_uncommitted_service() {
        let fixture = Fixture::new();
        let cancelled = Cell::new(false);
        let first = fixture.generations.issue();
        let next = Cell::new(0);
        assert!(!fixture.start(first, &cancelled, || next.set(fixture.generations.issue())));
        assert_eq!(fixture.owner(), None);
        assert_eq!(*fixture.disposed.borrow(), vec![first]);
        assert!(fixture.start(next.get(), &cancelled, || {}));
        assert_eq!(fixture.owner(), Some(next.get()));
    }

    #[test]
    fn cancellation_during_publication_cannot_announce_ready() {
        let fixture = Fixture::new();
        let cancelled = Cell::new(false);
        let first = fixture.generations.issue();
        assert!(!fixture.start(first, &cancelled, || cancelled.set(true)));
        assert_eq!(fixture.owner(), None);
        assert_eq!(*fixture.disposed.borrow(), vec![first]);
    }
}
