//! Owned loopback subscriber. Slow message handling never owns its socket.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SUBSCRIBE_ID: &str = "consilium-subscribe";
pub(super) const SUBSCRIBE_REQUEST: &str = "{\"jsonrpc\":\"2.0\",\"id\":\"consilium-subscribe\",\"method\":\"subscribeReceive\"}\n";

#[derive(Clone, Copy)]
struct Options {
    queue_capacity: usize,
    frame_limit: usize,
    poll: Duration,
    connect_timeout: Duration,
    rpc_timeout: Duration,
    health_interval: Duration,
    retry_delay: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Self { queue_capacity: 64, frame_limit: 512 * 1024,
            poll: Duration::from_millis(100), connect_timeout: Duration::from_secs(5),
            rpc_timeout: Duration::from_secs(5), health_interval: Duration::from_secs(15),
            retry_delay: Duration::from_secs(1) }
    }
}

struct Control {
    stop: AtomicBool,
    ready: AtomicBool,
    connect_failed: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}

pub(super) struct Subscription<T> {
    messages: mpsc::Receiver<T>,
    control: Arc<Control>,
    task: Option<JoinHandle<()>>,
}
impl<T> Subscription<T> {
    pub(super) fn is_ready(&self) -> bool { self.control.ready.load(Ordering::Acquire) }
    pub(super) fn take_connect_failure(&self) -> bool { self.control.connect_failed.swap(false, Ordering::AcqRel) }
    pub(super) fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        self.messages.recv_timeout(timeout)
    }
}
impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Release);
        if let Some(socket) = self.control.socket.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(task) = self.task.take() { let _ = task.join(); }
    }
}

pub(super) fn start<T, Current, Decode, Ready>(endpoint: SocketAddr, current: Current, decode: Decode, ready: Ready) -> Result<Subscription<T>, String>
where
    T: Send + 'static,
    Current: Fn() -> Result<bool, String> + Send + 'static,
    Decode: Fn(&Value) -> Option<T> + Send + 'static,
    Ready: Fn(bool) + Send + 'static,
{
    start_with(endpoint, current, decode, ready, Options::default())
}

fn start_with<T, Current, Decode, Ready>(endpoint: SocketAddr, current: Current, decode: Decode, ready: Ready, options: Options) -> Result<Subscription<T>, String>
where
    T: Send + 'static,
    Current: Fn() -> Result<bool, String> + Send + 'static,
    Decode: Fn(&Value) -> Option<T> + Send + 'static,
    Ready: Fn(bool) + Send + 'static,
{
    if !endpoint.ip().is_loopback() { return Err("Signal subscriber requires a loopback endpoint".into()); }
    let control = Arc::new(Control { stop: AtomicBool::new(false), ready: AtomicBool::new(false), connect_failed: AtomicBool::new(false), socket: Mutex::new(None) });
    let (send, messages) = mpsc::sync_channel(options.queue_capacity);
    let shared = control.clone();
    ready(false);
    let task = thread::Builder::new().name("signal-receive".into()).spawn(move || {
        let mut pending = None;
        let mut last_log = None;
        loop {
            let attempt = (|| {
                check(&shared, &current)?;
                // A parsed message retained during EOF, an RPC timeout or
                // backpressure belongs to this unchanged configuration.
                offer(&mut pending, &send, &shared, &current, None, options.poll)?;
                connection(endpoint, Delivery { queue: &send, pending: &mut pending }, &shared, &current, &decode, &ready, options)
            })();
            match attempt {
                Err(Failure::Stopped) => break,
                Err(Failure::Retry(error)) => {
                    // Reconnect failures must not turn into a console/log pump.
                    if last_log.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(30)) {
                        eprintln!("signal subscriber: {error}");
                        last_log = Some(Instant::now());
                    }
                }
                Ok(()) => {}
            }
            if !pause(&shared, options.retry_delay, options.poll) { break; }
        }
        shared.ready.store(false, Ordering::Release);
        ready(false);
    }).map_err(|e| format!("Start Signal subscriber: {e}"))?;
    Ok(Subscription { messages, control, task: Some(task) })
}

#[derive(Debug)]
enum Failure { Stopped, Retry(String) }
impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self { Self::Retry(error.to_string()) }
}

fn check(control: &Control, current: &impl Fn() -> Result<bool, String>) -> Result<(), Failure> {
    if control.stop.load(Ordering::Acquire) { return Err(Failure::Stopped); }
    match current() {
        Ok(true) => Ok(()),
        Ok(false) => Err(Failure::Stopped),
        // A partially written config must pause receiving, not discard the
        // channel of already accepted messages or expose local account data.
        Err(_) => Err(Failure::Retry("Signal configuration is temporarily unreadable".into())),
    }
}

fn pause(control: &Control, delay: Duration, poll: Duration) -> bool {
    let until = Instant::now() + delay;
    while Instant::now() < until {
        if control.stop.load(Ordering::Acquire) { return false; }
        thread::sleep(poll.min(until.saturating_duration_since(Instant::now())));
    }
    !control.stop.load(Ordering::Acquire)
}

fn offer<T>(pending: &mut Option<T>, send: &mpsc::SyncSender<T>, control: &Control,
    current: &impl Fn() -> Result<bool, String>, deadline: Option<Instant>, poll: Duration) -> Result<(), Failure> {
    while let Some(message) = pending.take() {
        match send.try_send(message) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Disconnected(message)) => {
                *pending = Some(message);
                return Err(Failure::Stopped);
            }
            Err(mpsc::TrySendError::Full(message)) => {
                *pending = Some(message);
                check(control, current)?;
                if deadline.is_some_and(|until| Instant::now() >= until) {
                    return Err(Failure::Retry("Signal RPC deadline expired during receiver backpressure".into()));
                }
                thread::sleep(poll);
            }
        }
    }
    Ok(())
}

struct Connected<'a, Ready: Fn(bool)> { control: &'a Control, ready: &'a Ready }
impl<Ready: Fn(bool)> Drop for Connected<'_, Ready> {
    fn drop(&mut self) {
        self.control.ready.store(false, Ordering::Release);
        (self.ready)(false);
        if let Some(socket) = self.control.socket.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

struct Delivery<'a, T> {
    queue: &'a mpsc::SyncSender<T>,
    pending: &'a mut Option<T>,
}

fn connection<T>(endpoint: SocketAddr, delivery: Delivery<'_, T>,
    control: &Control, current: &impl Fn() -> Result<bool, String>, decode: &impl Fn(&Value) -> Option<T>,
    ready: &impl Fn(bool), options: Options) -> Result<(), Failure> {
    check(control, current)?;
    let stream = TcpStream::connect_timeout(&endpoint, options.connect_timeout).map_err(|error| {
        control.connect_failed.store(true, Ordering::Release);
        Failure::from(error)
    })?;
    stream.set_read_timeout(Some(options.poll))?;
    stream.set_write_timeout(Some(options.rpc_timeout.min(Duration::from_secs(2))))?;
    *control.socket.lock().unwrap_or_else(|e| e.into_inner()) = Some(stream.try_clone()?);
    let _connected = Connected { control, ready };
    check(control, current)?;
    (&stream).write_all(SUBSCRIBE_REQUEST.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    let subscribe_until = Instant::now() + options.rpc_timeout;
    let mut subscribed = false;
    let mut next_health = Instant::now() + options.health_interval;
    let mut health: Option<(String, Instant)> = None;
    let mut health_id = 0u64;
    loop {
        check(control, current)?;
        let now = Instant::now();
        if !subscribed && now >= subscribe_until {
            return Err(Failure::Retry("Signal did not acknowledge subscribeReceive before its deadline".into()));
        }
        if health.as_ref().is_some_and(|(_, until)| now >= *until) {
            return Err(Failure::Retry("Signal subscriber stopped answering its same-session health request".into()));
        }
        if subscribed && health.is_none() && now >= next_health {
            health_id = health_id.wrapping_add(1);
            let id = format!("consilium-listener-health-{health_id}");
            let request = json!({"jsonrpc": "2.0", "id": id, "method": "version"});
            let mut writer = reader.get_ref();
            writer.write_all(format!("{request}\n").as_bytes())?;
            health = Some((id, Instant::now() + options.rpc_timeout));
        }
        let deadline = if !subscribed { Some(subscribe_until) } else { Some(health.as_ref().map_or(next_health + options.rpc_timeout, |(_, until)| *until)) };
        offer(delivery.pending, delivery.queue, control, current, deadline, options.poll)?;
        let Some(line) = read_frame(&mut reader, &mut bytes, options.frame_limit)? else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if value.get("id").and_then(Value::as_str) == Some(SUBSCRIBE_ID) {
            if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || value.get("error").is_some()
                || value.get("result").and_then(Value::as_i64).is_none_or(|id| id < 0) {
                return Err(Failure::Retry("Signal rejected or malformed the subscribeReceive acknowledgement".into()));
            }
            if !subscribed {
                subscribed = true;
                next_health = Instant::now() + options.health_interval;
                control.ready.store(true, Ordering::Release);
                ready(true);
            }
            continue;
        }
        if let Some((id, _)) = &health {
            if value.get("id").and_then(Value::as_str) == Some(id.as_str()) {
                if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || value.get("error").is_some()
                    || value.pointer("/result/version").and_then(Value::as_str).is_none_or(str::is_empty) {
                    return Err(Failure::Retry("Signal rejected the subscriber health request".into()));
                }
                health = None;
                next_health = Instant::now() + options.health_interval;
                continue;
            }
        }
        // Notifications may precede the subscription response or race a
        // health reply. Preserve them without treating them as RPC success.
        if let Some(message) = decode(&value) { *delivery.pending = Some(message); }
    }
}

/// Read one available fragment, retaining incomplete frames across timeouts.
/// Returning between fragments also bounds RPC deadlines under constant output.
fn read_frame(reader: &mut impl BufRead, bytes: &mut Vec<u8>, limit: usize) -> Result<Option<String>, Failure> {
    let input = match reader.fill_buf() {
        Ok([]) => return Err(Failure::Retry("Signal closed the subscriber connection".into())),
        Ok(input) => input,
        Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let length = input.iter().position(|byte| *byte == b'\n').map_or(input.len(), |index| index + 1);
    if bytes.len().saturating_add(length) > limit {
        return Err(Failure::Retry("Signal subscriber frame exceeded its size limit".into()));
    }
    let complete = input[length - 1] == b'\n';
    bytes.extend_from_slice(&input[..length]);
    reader.consume(length);
    if !complete { return Ok(None); }
    let line = String::from_utf8(std::mem::take(bytes)).map_err(|_| Failure::Retry("Signal subscriber frame is not UTF-8".into()))?;
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;

    fn options() -> Options {
        Options { queue_capacity: 2, frame_limit: 2048, poll: Duration::from_millis(10),
            connect_timeout: Duration::from_millis(300), rpc_timeout: Duration::from_millis(250),
            health_interval: Duration::from_millis(150), retry_delay: Duration::from_millis(20) }
    }
    fn listener() -> TcpListener {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    }
    fn accept(listener: &TcpListener) -> TcpStream {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((stream, _)) => { stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap(); return stream; }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("fake daemon accept: {error}"),
            }
        }
    }
    fn read(reader: &mut impl BufRead) -> Value {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }
    fn write(stream: &mut TcpStream, value: Value) { writeln!(stream, "{value}").unwrap(); }
    fn ack(stream: &mut TcpStream) { write(stream, json!({"jsonrpc":"2.0", "id":SUBSCRIBE_ID, "result":0})); }
    fn event(stream: &mut TcpStream, number: u64) { write(stream, json!({"method":"receive", "params":{"number":number}})); }
    fn decode(value: &Value) -> Option<u64> {
        (value.get("method").and_then(Value::as_str) == Some("receive"))
            .then(|| value.pointer("/params/number").and_then(Value::as_u64)).flatten()
    }
    fn wait(mut condition: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(3);
        while !condition() { assert!(Instant::now() < until, "fake socket condition timed out"); thread::sleep(Duration::from_millis(5)); }
    }
    fn eof(reader: &mut impl BufRead) {
        let mut rest = Vec::new();
        match reader.read_to_end(&mut rest) {
            Ok(_) => {},
            Err(error) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof) => {},
            Err(error) => panic!("fake daemon did not observe socket close: {error}"),
        }
    }

    #[test]
    fn only_a_matching_integer_subscription_acknowledgement_marks_ready() {
        for response in [json!({"jsonrpc":"2.0","id":SUBSCRIBE_ID,"result":null}),
            json!({"jsonrpc":"2.0","id":SUBSCRIBE_ID,"result":{}}),
            json!({"jsonrpc":"2.0","id":SUBSCRIBE_ID,"result":true}),
            json!({"jsonrpc":"2.0","id":SUBSCRIBE_ID,"result":-1}),
            json!({"id":SUBSCRIBE_ID,"result":0}),
            json!({"jsonrpc":"2.0","id":SUBSCRIBE_ID,"result":0,"error":{"message":"fixture"}}),
            json!({"jsonrpc":"2.0","id":"wrong-id","result":0})] {
            let listener = listener();
            let endpoint = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let mut stream = accept(&listener);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                assert_eq!(read(&mut reader)["method"], "subscribeReceive");
                write(&mut stream, response);
                eof(&mut reader);
            });
            let count = Arc::new(AtomicUsize::new(0));
            let seen = count.clone();
            let subscription = start_with(endpoint, || Ok(true), decode, move |ready| { if ready { seen.fetch_add(1, Ordering::SeqCst); } }, options()).unwrap();
            server.join().unwrap();
            drop(subscription);
            assert_eq!(count.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn notifications_before_the_ack_are_preserved_without_proving_readiness() {
        let listener = listener();
        let endpoint = listener.local_addr().unwrap();
        let (release, ready_to_ack) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            read(&mut reader);
            event(&mut stream, 7);
            ready_to_ack.recv_timeout(Duration::from_secs(2)).unwrap();
            ack(&mut stream);
            eof(&mut reader);
        });
        let subscription = start_with(endpoint, || Ok(true), decode, |_| {}, options()).unwrap();
        assert_eq!(subscription.recv_timeout(Duration::from_secs(2)).unwrap(), 7);
        assert!(!subscription.is_ready());
        release.send(()).unwrap();
        wait(|| subscription.is_ready());
        drop(subscription);
        server.join().unwrap();
    }

    #[test]
    fn a_slow_worker_and_eof_do_not_discard_already_accepted_messages() {
        let listener = listener();
        let endpoint = listener.local_addr().unwrap();
        let (written, finished_writing) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut first = accept(&listener);
            read(&mut BufReader::new(first.try_clone().unwrap()));
            ack(&mut first);
            for number in 0..4 { event(&mut first, number); }
            first.shutdown(Shutdown::Both).unwrap();
            written.send(()).unwrap();
            let mut second = accept(&listener);
            let mut reader = BufReader::new(second.try_clone().unwrap());
            read(&mut reader);
            ack(&mut second);
            event(&mut second, 4);
            eof(&mut reader);
        });
        let subscription = start_with(endpoint, || Ok(true), decode, |_| {}, options()).unwrap();
        finished_writing.recv_timeout(Duration::from_secs(2)).unwrap();
        thread::sleep(Duration::from_millis(30)); // hold the worker beyond the two-slot channel
        let received: Vec<_> = (0..5).map(|_| subscription.recv_timeout(Duration::from_secs(2)).unwrap()).collect();
        assert_eq!(received, vec![0, 1, 2, 3, 4]);
        drop(subscription);
        server.join().unwrap();
    }

    #[test]
    fn same_session_health_times_out_despite_unrelated_notifications_and_reconnects() {
        let listener = listener();
        let endpoint = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let server = thread::spawn(move || {
            let mut first = accept(&listener);
            let mut reader = BufReader::new(first.try_clone().unwrap());
            read(&mut reader); ack(&mut first);
            let health = read(&mut reader);
            assert_eq!(health["method"], "version");
            write(&mut first, json!({"jsonrpc":"2.0","id":"unrelated", "result":{"version":"fixture"}}));
            event(&mut first, 42);
            eof(&mut reader); // unanswered health request must close this silent session
            let mut second = accept(&listener);
            let mut reader = BufReader::new(second.try_clone().unwrap());
            read(&mut reader); ack(&mut second); event(&mut second, 43);
            eof(&mut reader);
        });
        let subscription = start_with(endpoint, || Ok(true), decode, move |ready| { if ready { seen.fetch_add(1, Ordering::SeqCst); } }, options()).unwrap();
        assert_eq!(subscription.recv_timeout(Duration::from_secs(2)).unwrap(), 42);
        assert_eq!(subscription.recv_timeout(Duration::from_secs(2)).unwrap(), 43);
        assert!(count.load(Ordering::SeqCst) >= 2);
        drop(subscription);
        server.join().unwrap();
    }

    #[test]
    fn a_notification_interleaved_with_the_matching_health_reply_is_retained() {
        let listener = listener();
        let endpoint = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            read(&mut reader); ack(&mut stream);
            let health = read(&mut reader);
            event(&mut stream, 81);
            write(&mut stream, json!({"jsonrpc":"2.0","id":health["id"], "result":{"version":"fixture"}}));
            eof(&mut reader);
        });
        let subscription = start_with(endpoint, || Ok(true), decode, |_| {}, options()).unwrap();
        assert_eq!(subscription.recv_timeout(Duration::from_secs(2)).unwrap(), 81);
        assert!(subscription.is_ready());
        drop(subscription);
        server.join().unwrap();
    }

    #[test]
    fn transient_configuration_read_failure_preserves_the_queue_for_reconnection() {
        let listener = listener();
        let endpoint = listener.local_addr().unwrap();
        let phase = Arc::new(AtomicUsize::new(0));
        let current = phase.clone();
        let count = Arc::new(AtomicUsize::new(0)); let seen = count.clone();
        let decoded = Arc::new(AtomicBool::new(false)); let accepted = decoded.clone();
        let server = thread::spawn(move || {
            let mut first = accept(&listener);
            let mut reader = BufReader::new(first.try_clone().unwrap());
            read(&mut reader); ack(&mut first); event(&mut first, 9); eof(&mut reader);
            let mut second = accept(&listener);
            let mut reader = BufReader::new(second.try_clone().unwrap());
            read(&mut reader); ack(&mut second); event(&mut second, 10); eof(&mut reader);
        });
        let subscription = start_with(endpoint, move || if current.load(Ordering::SeqCst) == 1 { Err("fixture config write".into()) } else { Ok(true) }, move |value| {
            let message = decode(value);
            if message == Some(9) { accepted.store(true, Ordering::SeqCst); }
            message
        }, move |ready| { if ready { seen.fetch_add(1, Ordering::SeqCst); } }, options()).unwrap();
        wait(|| subscription.is_ready() && decoded.load(Ordering::SeqCst));
        phase.store(1, Ordering::SeqCst);
        wait(|| !subscription.is_ready());
        phase.store(2, Ordering::SeqCst);
        wait(|| count.load(Ordering::SeqCst) >= 2);
        assert_eq!(subscription.recv_timeout(Duration::from_secs(2)).unwrap(), 9);
        assert_eq!(subscription.recv_timeout(Duration::from_secs(2)).unwrap(), 10);
        drop(subscription);
        server.join().unwrap();
    }

    #[test]
    fn dropping_a_backpressured_subscriber_joins_its_only_reader_thread() {
        let listener = listener(); let endpoint = listener.local_addr().unwrap();
        let (written, full) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut stream = accept(&listener); let mut reader = BufReader::new(stream.try_clone().unwrap());
            read(&mut reader); ack(&mut stream);
            for number in 0..5 { event(&mut stream, number); }
            written.send(()).unwrap(); eof(&mut reader);
        });
        let subscription = start_with(endpoint, || Ok(true), decode, |_| {}, options()).unwrap();
        full.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now(); drop(subscription);
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }

    struct TimeoutFragments { bytes: Cursor<Vec<u8>>, reads: usize }
    impl Read for TimeoutFragments {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            if self.reads == 2 { return Err(std::io::ErrorKind::TimedOut.into()); }
            let length = output.len().min(3); self.bytes.read(&mut output[..length])
        }
    }
    #[test]
    fn fragmented_utf8_survives_timeouts_and_oversized_frames_fail_closed() {
        let expected = "{\"message\":\"hÃ©llo\"}\n";
        let mut reader = BufReader::new(TimeoutFragments { bytes: Cursor::new(expected.as_bytes().to_vec()), reads: 0 });
        let mut bytes = Vec::new(); let mut received = None;
        for _ in 0..30 { if let Some(frame) = read_frame(&mut reader, &mut bytes, 100).unwrap() { received = Some(frame); break; } }
        assert_eq!(received.as_deref(), Some(expected));
        let mut reader = Cursor::new(vec![b'x'; 101]);
        assert!(matches!(read_frame(&mut reader, &mut Vec::new(), 100), Err(Failure::Retry(_))));
    }

    #[test]
    fn refused_loopback_connections_request_existing_revive_without_marking_ready() {
        let unused = listener(); let endpoint = unused.local_addr().unwrap(); drop(unused);
        let subscription = start_with(endpoint, || Ok(true), decode, |_| {}, options()).unwrap();
        wait(|| subscription.take_connect_failure());
        assert!(!subscription.is_ready());
        drop(subscription);
    }

    #[test]
    fn non_loopback_endpoints_are_refused_before_starting_a_thread() {
        assert!(start("203.0.113.1:7583".parse().unwrap(), || Ok(true), decode, |_| {}).is_err());
    }
}
