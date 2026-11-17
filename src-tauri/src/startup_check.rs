//! The startup check: each time Consilium starts, it checks every connection it depends on
//! (Signal, linked computers, each council AI, the internet and GitHub's checks), posts the result
//! as a Consilium note in the newest conversation and sends it to the phone's Note to Self.
//! It waits for Signal and linked computers to come up first, so something that is merely still
//! starting isn't reported as broken.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::bridge::{self, BridgeConfig};
use crate::engine::Engine;
use crate::store::now_ms;

/// Every startup report starts with this; Signal mirroring skips notes that start with it,
/// because the report is sent to the phone directly.
pub const HEADER: &str = "Startup check";

/// How long to wait for Signal and linked computers to connect before reporting.
const SETTLE: Duration = Duration::from_secs(60);
/// The shortest wait, so the first connection attempts have finished.
const MIN_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Ok,
    /// Works, but needs attention soon (out of usage, checks still running).
    Warn,
    Bad,
    /// For information only: not set up, or nothing to do.
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub mark: Mark,
    pub text: String,
}

fn line(mark: Mark, text: impl Into<String>) -> Line {
    Line { mark, text: text.into() }
}

/// The report text for the chat and the phone.
pub fn render(device: &str, version: &str, lines: &[Line]) -> String {
    let problems = lines.iter().filter(|l| l.mark == Mark::Bad).count();
    let warnings = lines.iter().filter(|l| l.mark == Mark::Warn).count();
    let summary = match (problems, warnings) {
        (0, 0) => "everything is working.".to_string(),
        (0, w) => format!("everything is connected; {w} item(s) need attention."),
        (p, _) => format!("{p} problem(s) found."),
    };
    let mut out = format!("{HEADER} on {device} (Consilium {version}): {summary}");
    for l in lines {
        let sym = match l.mark {
            Mark::Ok => "✓",
            Mark::Warn => "!",
            Mark::Bad => "✗",
            Mark::Info => "·",
        };
        out.push_str(&format!("\n{sym} {}", l.text));
    }
    out
}

/// What is known about Signal on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalState {
    NotSetUp,
    Off,
    Connected,
    NotConnected,
}

fn signal_state() -> SignalState {
    let Ok(cfg) = BridgeConfig::load(&bridge::config_path(None)) else { return SignalState::NotSetUp };
    if !bridge::valid_account(&cfg.account) || cfg.allowed_acis.is_empty() {
        return SignalState::NotSetUp;
    }
    if !cfg.enabled {
        return SignalState::Off;
    }
    if crate::link::signal_on() { SignalState::Connected } else { SignalState::NotConnected }
}

pub fn signal_line(s: SignalState) -> Line {
    match s {
        SignalState::NotSetUp => line(Mark::Info, "Signal: not set up (Settings → Signal)."),
        SignalState::Off => line(Mark::Info, "Signal: switched off in Settings."),
        SignalState::Connected => line(Mark::Ok, "Signal: connected; phone messages reach the council."),
        SignalState::NotConnected => line(
            Mark::Bad,
            "Signal: set up but not connected to signal-cli. Phone messages won't arrive until it reconnects; Settings → Signal shows why.",
        ),
    }
}

fn ago(ms: u64) -> String {
    let s = ms / 1000;
    if s < 90 {
        format!("{s} s ago")
    } else if s < 5400 {
        format!("{} min ago", s / 60)
    } else {
        format!("{} h ago", s / 3600)
    }
}

pub fn link_lines(st: &crate::link::LinkStatus, now: u64) -> Vec<Line> {
    if st.peers.is_empty() {
        return vec![line(Mark::Info, "Linked computers: none paired.")];
    }
    if !st.enabled {
        return vec![line(
            Mark::Warn,
            format!("Linked computers: {} paired, but the link is switched off in Settings.", st.peers.len()),
        )];
    }
    let names: Vec<(String, String)> = st.peers.iter().map(|p| (p.node_id.clone(), p.name.clone())).collect();
    st.peers
        .iter()
        .map(|p| {
            let name = crate::link::nickname(&p.node_id, &names);
            let routes = p.addrs.len();
            let routes = if routes == 1 { "1 route known".to_string() } else { format!("{routes} routes known") };
            if p.connected {
                let via = p.route.as_deref().map(|r| format!(" via {r}")).unwrap_or_default();
                let rtt = p.rtt_ms.map(|r| format!(" ({r} ms)")).unwrap_or_default();
                let queued = if p.queued > 0 { format!("; {} message(s) still waiting to send", p.queued) } else { String::new() };
                let mark = if p.queued > 0 { Mark::Warn } else { Mark::Ok };
                line(mark, format!("{name}: connected{via}{rtt}, {routes}{queued}."))
            } else {
                let seen = p.last_seen_ms.map(|t| format!(" (last seen {})", ago(now.saturating_sub(t)))).unwrap_or_default();
                let queued = if p.queued > 0 { format!("; {} message(s) waiting for it", p.queued) } else { String::new() };
                line(Mark::Bad, format!("{name}: not connected{seen}, {routes}{queued}."))
            }
        })
        .collect()
}

pub fn agent_line(info: &crate::agents::AgentInfo, benched_until: Option<u64>, now: u64) -> Line {
    let name = &info.name;
    if !info.available {
        return line(Mark::Bad, format!("{name}: {} not found. {}", info.tool, info.hint).trim_end().to_string());
    }
    if info.signed_in == Some(false) {
        return line(Mark::Bad, format!("{name}: not signed in. {}", info.hint).trim_end().to_string());
    }
    if let Some(until) = benched_until.filter(|&u| u > now) {
        return line(Mark::Warn, format!("{name}: out of usage until {}.", crate::usage::when(until, now)));
    }
    line(Mark::Ok, format!("{name}: ready."))
}

pub fn ci_line(s: &crate::ci::CiStatus) -> Option<Line> {
    let at = s.commit.as_deref().map(|c| format!(" @{c}")).unwrap_or_default();
    let what = format!("GitHub checks for {} {}{at}", s.project, s.branch);
    Some(match s.state.as_str() {
        "pass" => line(Mark::Ok, format!("{what}: passing.")),
        "running" => line(Mark::Warn, format!("{what}: still running.")),
        "fail" => line(Mark::Bad, format!("{what}: failing ({}).", s.failed.join(", "))),
        "error" => line(Mark::Warn, format!("{what}: couldn't be read (is the GitHub CLI signed in?).")),
        _ => return None,
    })
}

fn internet_line() -> Line {
    use std::net::{TcpStream, ToSocketAddrs};
    let reachable = ("github.com", 443)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_secs(4)).is_ok());
    if reachable {
        line(Mark::Ok, "Internet: reachable.")
    } else {
        line(Mark::Bad, "Internet: github.com couldn't be reached, so online AIs and GitHub won't work.")
    }
}

/// Whether Signal and every paired computer are up, so the report can go out early.
fn settled() -> bool {
    let signal_ok = signal_state() != SignalState::NotConnected;
    let link_ok = crate::link_handle().is_none_or(|l| {
        let st = l.status();
        !st.enabled || st.peers.iter().all(|p| p.connected)
    });
    signal_ok && link_ok
}

async fn gather(engine: &Arc<Engine>, data: &Path, workspace: Option<PathBuf>) -> Vec<Line> {
    let now = now_ms();
    let mut lines = vec![signal_line(signal_state())];
    if let Some(l) = crate::link_handle() {
        lines.extend(link_lines(&l.status(), now));
    }
    // The council's AIs, checked at the same time so a slow CLI doesn't hold up the others.
    let config = engine.config.clone();
    let members = config.get().members;
    let mut tasks = tokio::task::JoinSet::new();
    for (i, id) in members.iter().enumerate() {
        if let Some(agent) = crate::agents::by_id(id) {
            let access = config.access(agent.id);
            tasks.spawn(async move { (i, crate::agents::status(agent, true, access).await) });
        }
    }
    let mut ais = Vec::new();
    while let Some(Ok((i, info))) = tasks.join_next().await {
        let benched = engine.bench.until(&info.id, now);
        ais.push((i, agent_line(&info, benched, now)));
    }
    ais.sort_by_key(|(i, _)| *i);
    if ais.is_empty() {
        lines.push(line(Mark::Info, "AIs: nobody is in the council yet."));
    }
    lines.extend(ais.into_iter().map(|(_, l)| l));
    let blocking = tauri::async_runtime::spawn_blocking(move || {
        let mut l = vec![internet_line()];
        if let Some(ws) = workspace {
            l.extend(crate::ci::for_workspace(&ws).iter().filter_map(ci_line));
        }
        l
    })
    .await
    .unwrap_or_default();
    lines.extend(blocking);
    if let Some(s) = crate::update::staged(data) {
        lines.push(line(Mark::Info, format!("Update Now · {} is ready to install (you have {}).", s.version, s.current)));
    }
    lines
}

/// Runs once per app start: waits for connections to settle, then posts and sends the report.
pub async fn run(engine: Arc<Engine>, data: PathBuf) {
    let started = std::time::Instant::now();
    tokio::time::sleep(MIN_WAIT).await;
    while started.elapsed() < SETTLE && !settled() {
        tokio::time::sleep(POLL).await;
    }
    let conv = engine.store.list().into_iter().find(|c| !crate::peer_sync::is_mirror(&c.id));
    let workspace = conv
        .as_ref()
        .and_then(|c| c.workspace.clone())
        .or_else(|| engine.config.get().default_workspace)
        .filter(|w| !w.is_empty())
        .map(PathBuf::from);
    let lines = gather(&engine, &data, workspace).await;
    let report = render(&crate::signal_setup::device_name(), env!("CARGO_PKG_VERSION"), &lines);
    if let Some(c) = conv {
        let _ = engine.note(&c.id, &report);
    }
    if let Err(e) = tauri::async_runtime::spawn_blocking(move || crate::signal_chat::send_to_phone(&report))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
    {
        eprintln!("startup check: not sent to Signal: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::{LinkStatus, PeerView};

    fn peer(name: &str, connected: bool, queued: usize) -> PeerView {
        PeerView {
            node_id: format!("node-{name}"),
            name: name.into(),
            connected,
            route: connected.then(|| "192.168.1.157:8470".into()),
            rtt_ms: connected.then_some(3),
            last_seen_ms: Some(1_000),
            queued,
            activity: vec![],
            git: None,
            build: None,
            handshake: vec![],
            addrs: vec!["192.168.1.157:8470".into(), "192.168.1.170:8470".into()],
        }
    }

    fn status(enabled: bool, peers: Vec<PeerView>) -> LinkStatus {
        LinkStatus {
            enabled,
            node_id: "me".into(),
            name: "Mac".into(),
            port: 8470,
            addrs: vec![],
            pairing_code_expires_ms: None,
            peers,
            error: None,
            build: crate::build_id::Build::this(),
            follow: crate::follow::status(),
        }
    }

    #[test]
    fn report_counts_problems_and_marks_each_line() {
        let all_ok = render("Mac", "1.0", &[line(Mark::Ok, "Signal: connected."), line(Mark::Info, "x")]);
        assert!(all_ok.starts_with("Startup check on Mac (Consilium 1.0): everything is working."));
        assert!(all_ok.contains("\n✓ Signal: connected."));
        let warn = render("Mac", "1.0", &[line(Mark::Warn, "a")]);
        assert!(warn.contains("1 item(s) need attention"));
        let bad = render("Mac", "1.0", &[line(Mark::Bad, "a"), line(Mark::Bad, "b"), line(Mark::Warn, "c")]);
        assert!(bad.contains("2 problem(s) found."));
        assert!(bad.contains("\n✗ a\n✗ b\n! c"));
    }

    #[test]
    fn linked_computers_report_route_and_drops() {
        let now = 1_000 + 120_000;
        let lines = link_lines(&status(true, vec![peer("PC", true, 0), peer("Laptop", false, 2)]), now);
        assert_eq!(lines[0].mark, Mark::Ok);
        assert!(lines[0].text.contains("PC: connected via 192.168.1.157:8470 (3 ms), 2 routes known."));
        assert_eq!(lines[1].mark, Mark::Bad);
        assert!(lines[1].text.contains("not connected (last seen 2 min ago)"));
        assert!(lines[1].text.contains("2 message(s) waiting for it"));
        assert_eq!(link_lines(&status(true, vec![]), now)[0].mark, Mark::Info);
        assert_eq!(link_lines(&status(false, vec![peer("PC", false, 0)]), now)[0].mark, Mark::Warn);
    }

    #[test]
    fn failing_checks_are_problems_and_unknown_ones_are_left_out() {
        let mut s = crate::ci::CiStatus {
            project: "app".into(),
            path: "/x".into(),
            branch: "main".into(),
            commit: Some("abc1234".into()),
            state: "fail".into(),
            failed: vec!["CI › Rust (windows)".into()],
            runs: 1,
            url: None,
            detail: None,
            checked_at: 0,
        };
        let l = ci_line(&s).unwrap();
        assert_eq!(l.mark, Mark::Bad);
        assert!(l.text.contains("app main @abc1234: failing (CI › Rust (windows))"));
        s.state = "none".into();
        assert!(ci_line(&s).is_none());
    }

    #[test]
    fn signal_lines_match_their_state() {
        assert_eq!(signal_line(SignalState::Connected).mark, Mark::Ok);
        assert_eq!(signal_line(SignalState::NotConnected).mark, Mark::Bad);
        assert_eq!(signal_line(SignalState::NotSetUp).mark, Mark::Info);
    }
}
