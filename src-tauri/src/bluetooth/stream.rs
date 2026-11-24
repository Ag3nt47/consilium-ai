//! Bounded byte queues between native Bluetooth callbacks and the async protocol.
//! One native writer owns each pump. Bytes leave its outgoing queue only after
//! the native API confirms the write; an unfinished write cannot pass flush.
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(super) const BUFFER_LIMIT: usize = 64 * 1024;
type WakeNative = Arc<dyn Fn() + Send + Sync>;

pub(super) fn channel(wake: WakeNative) -> (NativeStream, NativePump) {
    let shared = Arc::new(Shared { state: Mutex::new(State::default()), cancelled: AtomicBool::new(false), wake });
    (NativeStream { shared: shared.clone() }, NativePump { shared })
}

#[derive(Default)]
struct State {
    incoming: VecDeque<u8>,
    outgoing: VecDeque<u8>,
    read_eof: bool,
    closed: bool,
    shutdown_requested: bool,
    shutdown_done: bool,
    failure: Option<String>,
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
    flush_waker: Option<Waker>,
    shutdown_waker: Option<Waker>,
}
struct Shared {
    state: Mutex<State>,
    cancelled: AtomicBool,
    wake: WakeNative,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> { self.state.lock().unwrap_or_else(|poison| poison.into_inner()) }
    fn failure(state: &State) -> Option<io::Error> {
        state.failure.as_ref().map(|message| io::Error::new(io::ErrorKind::ConnectionAborted, message.clone()))
    }
}

pub(super) struct NativeStream { shared: Arc<Shared> }
#[derive(Clone)]
pub(super) struct NativePump { shared: Arc<Shared> }

impl NativePump {
    pub(super) fn read_capacity(&self) -> usize {
        let state = self.shared.lock();
        if state.closed || state.read_eof || self.shared.cancelled.load(Ordering::Acquire) { 0 }
        else { BUFFER_LIMIT - state.incoming.len() }
    }

    /// The whole callback payload is enqueued, or none of it is. A caller unable
    /// to pause reception must close on WouldBlock instead of dropping bytes.
    pub(super) fn incoming(&self, input: &[u8]) -> io::Result<()> {
        let waker = {
            let mut state = self.shared.lock();
            if state.closed || state.read_eof || self.shared.cancelled.load(Ordering::Acquire) { return Err(io::ErrorKind::BrokenPipe.into()); }
            if input.len() > BUFFER_LIMIT - state.incoming.len() { return Err(io::ErrorKind::WouldBlock.into()); }
            state.incoming.extend(input);
            state.read_waker.take()
        };
        if let Some(waker) = waker { waker.wake(); }
        Ok(())
    }

    /// Peek one native write. Retain its returned buffer until completion and
    /// do not start another write until consume_written has acknowledged it.
    pub(super) fn outgoing(&self, max: usize) -> Option<Vec<u8>> {
        let state = self.shared.lock();
        let length = max.min(state.outgoing.len());
        if state.closed || self.shared.cancelled.load(Ordering::Acquire) || length == 0 { None } else { Some(state.outgoing.iter().take(length).copied().collect()) }
    }

    pub(super) fn consume_written(&self, length: usize) -> io::Result<()> {
        let (wakers, shutdown_ready) = {
            let mut state = self.shared.lock();
            if state.closed { return Err(io::ErrorKind::BrokenPipe.into()); }
            if length > state.outgoing.len() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "Native Bluetooth write exceeded the queued bytes.")); }
            state.outgoing.drain(..length);
            let flushed = if state.outgoing.is_empty() { state.flush_waker.take() } else { None };
            ([state.write_waker.take(), flushed], state.shutdown_requested && state.outgoing.is_empty())
        };
        for waker in wakers.into_iter().flatten() { waker.wake(); }
        if shutdown_ready { (self.shared.wake)(); }
        Ok(())
    }

    pub(super) fn end_read(&self) {
        let waker = {
            let mut state = self.shared.lock();
            state.read_eof = true;
            state.read_waker.take()
        };
        if let Some(waker) = waker { waker.wake(); }
    }

    pub(super) fn finish(&self, error: Option<String>) {
        let wakers = {
            let mut state = self.shared.lock();
            if state.closed { return; }
            state.closed = true;
            state.read_eof = true;
            state.shutdown_done = true;
            state.failure = error.or_else(|| (!state.outgoing.is_empty()).then(|| "Bluetooth closed before queued data was written.".into()));
            state.outgoing.clear();
            [state.read_waker.take(), state.write_waker.take(), state.flush_waker.take(), state.shutdown_waker.take()]
        };
        for waker in wakers.into_iter().flatten() { waker.wake(); }
    }

    pub(super) fn is_closed(&self) -> bool { self.shared.cancelled.load(Ordering::Acquire) || self.shared.lock().closed }
    pub(super) fn shutdown_requested(&self) -> bool { self.shared.lock().shutdown_requested }
    pub(super) fn shutdown_ready(&self) -> bool {
        let state = self.shared.lock();
        state.shutdown_requested && !state.shutdown_done && state.outgoing.is_empty()
    }
    pub(super) fn complete_shutdown(&self) {
        let waker = {
            let mut state = self.shared.lock();
            if !state.shutdown_requested || !state.outgoing.is_empty() { return; }
            state.shutdown_done = true;
            state.shutdown_waker.take()
        };
        if let Some(waker) = waker { waker.wake(); }
    }
}

impl Drop for NativeStream {
    fn drop(&mut self) {
        if !self.shared.cancelled.swap(true, Ordering::AcqRel) { (self.shared.wake)(); }
    }
}

impl AsyncRead for NativeStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, output: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if output.remaining() == 0 { return Poll::Ready(Ok(())); }
        let mut state = self.shared.lock();
        if !state.incoming.is_empty() {
            let length = output.remaining().min(state.incoming.len());
            let (first, second) = state.incoming.as_slices();
            let head = length.min(first.len());
            output.put_slice(&first[..head]);
            output.put_slice(&second[..length - head]);
            state.incoming.drain(..length);
            drop(state);
            (self.shared.wake)();
            return Poll::Ready(Ok(()));
        }
        if let Some(error) = Shared::failure(&state) { return Poll::Ready(Err(error)); }
        if state.read_eof { return Poll::Ready(Ok(())); }
        state.read_waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl AsyncWrite for NativeStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, input: &[u8]) -> Poll<io::Result<usize>> {
        if input.is_empty() { return Poll::Ready(Ok(0)); }
        let mut state = self.shared.lock();
        if let Some(error) = Shared::failure(&state) { return Poll::Ready(Err(error)); }
        if state.closed || state.shutdown_requested { return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())); }
        let length = input.len().min(BUFFER_LIMIT - state.outgoing.len());
        if length == 0 {
            state.write_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        state.outgoing.extend(&input[..length]);
        drop(state);
        (self.shared.wake)();
        Poll::Ready(Ok(length))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut state = self.shared.lock();
        if let Some(error) = Shared::failure(&state) { return Poll::Ready(Err(error)); }
        if state.outgoing.is_empty() { return Poll::Ready(Ok(())); }
        state.flush_waker = Some(cx.waker().clone());
        Poll::Pending
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut state = self.shared.lock();
        if let Some(error) = Shared::failure(&state) { return Poll::Ready(Err(error)); }
        if state.shutdown_done { return Poll::Ready(Ok(())); }
        state.shutdown_requested = true;
        state.shutdown_waker = Some(cx.waker().clone());
        drop(state);
        (self.shared.wake)();
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::task::Wake;

    #[derive(Default)]
    struct Counter(AtomicUsize);
    impl Wake for Counter { fn wake(self: Arc<Self>) { self.0.fetch_add(1, Ordering::SeqCst); } }
    fn setup() -> (NativeStream, NativePump, Arc<Counter>, Waker) {
        let native = Arc::new(Counter::default());
        let count = native.clone();
        let (stream, pump) = channel(Arc::new(move || { count.0.fetch_add(1, Ordering::SeqCst); }));
        let waker = Waker::from(Arc::new(Counter::default()));
        (stream, pump, native, waker)
    }

    #[test]
    fn flush_waits_for_native_completion_and_partial_writes_preserve_order() {
        let (mut stream, pump, _, waker) = setup();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(Pin::new(&mut stream).poll_write(&mut cx, b"abcdef"), Poll::Ready(Ok(6))));
        assert_eq!(pump.outgoing(3), Some(b"abc".to_vec()));
        assert_eq!(pump.outgoing(3), Some(b"abc".to_vec()), "Peeking never acknowledges the native write");
        assert!(Pin::new(&mut stream).poll_flush(&mut cx).is_pending());
        pump.consume_written(2).unwrap();
        assert_eq!(pump.outgoing(8), Some(b"cdef".to_vec()));
        assert!(Pin::new(&mut stream).poll_flush(&mut cx).is_pending());
        pump.consume_written(4).unwrap();
        assert!(matches!(Pin::new(&mut stream).poll_flush(&mut cx), Poll::Ready(Ok(()))));
    }

    #[test]
    fn outgoing_backpressure_resumes_after_native_consumption() {
        let (mut stream, pump, _, waker) = setup();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(Pin::new(&mut stream).poll_write(&mut cx, &vec![1; BUFFER_LIMIT + 1]), Poll::Ready(Ok(BUFFER_LIMIT))));
        assert!(Pin::new(&mut stream).poll_write(&mut cx, b"x").is_pending());
        pump.consume_written(1).unwrap();
        assert!(matches!(Pin::new(&mut stream).poll_write(&mut cx, b"xy"), Poll::Ready(Ok(1))));
        assert_eq!(pump.outgoing(BUFFER_LIMIT + 1).unwrap().len(), BUFFER_LIMIT);
    }

    #[test]
    fn incoming_overflow_is_atomic_and_queued_bytes_survive_it() {
        let (mut stream, pump, native, waker) = setup();
        pump.incoming(&vec![7; BUFFER_LIMIT - 2]).unwrap();
        assert_eq!(pump.incoming(b"abc").unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(pump.read_capacity(), 2);
        let mut bytes = vec![0; BUFFER_LIMIT];
        let mut output = ReadBuf::new(&mut bytes);
        assert!(matches!(Pin::new(&mut stream).poll_read(&mut Context::from_waker(&waker), &mut output), Poll::Ready(Ok(()))));
        assert_eq!(output.filled(), vec![7; BUFFER_LIMIT - 2]);
        assert_eq!(pump.read_capacity(), BUFFER_LIMIT);
        assert!(native.0.load(Ordering::SeqCst) > 0, "Reading wakes the native receiver to resume");
    }

    #[test]
    fn graceful_eof_delivers_buffered_data_before_ending_the_stream() {
        let (mut stream, pump, _, waker) = setup();
        pump.incoming(b"last").unwrap();
        pump.end_read();
        let mut bytes = [0; 8];
        let mut output = ReadBuf::new(&mut bytes);
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(Pin::new(&mut stream).poll_read(&mut cx, &mut output), Poll::Ready(Ok(()))));
        assert_eq!(output.filled(), b"last");
        let mut output = ReadBuf::new(&mut bytes);
        assert!(matches!(Pin::new(&mut stream).poll_read(&mut cx, &mut output), Poll::Ready(Ok(()))));
        assert!(output.filled().is_empty());
        assert_eq!(pump.incoming(b"late").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn shutdown_waits_for_the_native_write_and_native_shutdown_acknowledgement() {
        let (mut stream, pump, _, waker) = setup();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(Pin::new(&mut stream).poll_write(&mut cx, b"end"), Poll::Ready(Ok(3))));
        assert!(!pump.shutdown_requested());
        assert!(Pin::new(&mut stream).poll_shutdown(&mut cx).is_pending());
        assert!(pump.shutdown_requested());
        assert!(!pump.shutdown_ready());
        pump.complete_shutdown();
        assert!(Pin::new(&mut stream).poll_shutdown(&mut cx).is_pending());
        pump.consume_written(3).unwrap();
        assert!(pump.shutdown_ready());
        pump.complete_shutdown();
        assert!(matches!(Pin::new(&mut stream).poll_shutdown(&mut cx), Poll::Ready(Ok(()))));
        assert!(matches!(Pin::new(&mut stream).poll_write(&mut cx, b"x"), Poll::Ready(Err(_))));
    }

    #[test]
    fn native_close_with_unsent_bytes_cannot_report_a_successful_flush() {
        let (mut stream, pump, _, waker) = setup();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(Pin::new(&mut stream).poll_write(&mut cx, b"pending"), Poll::Ready(Ok(7))));
        pump.finish(None);
        assert!(matches!(Pin::new(&mut stream).poll_flush(&mut cx), Poll::Ready(Err(_))));
        assert!(pump.is_closed());
    }

    #[test]
    fn dropping_the_async_stream_wakes_native_cancellation() {
        let (stream, pump, native, _) = setup();
        assert!(!pump.is_closed());
        drop(stream);
        assert!(pump.is_closed());
        assert_eq!(native.0.load(Ordering::SeqCst), 1);
        assert_eq!(pump.read_capacity(), 0);
    }

    #[test]
    fn an_invalid_native_completion_never_consumes_queued_bytes() {
        let (mut stream, pump, _, waker) = setup();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(Pin::new(&mut stream).poll_write(&mut cx, b"abc"), Poll::Ready(Ok(3))));
        assert_eq!(pump.consume_written(4).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(pump.outgoing(8), Some(b"abc".to_vec()));
    }
}
