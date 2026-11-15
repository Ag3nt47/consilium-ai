//! Samples live host CPU and memory pressure for the 0-100% header gauge.
//!
//! `sysinfo` uses the native host APIs on macOS, Windows and Linux. Keeping one sampler alive is
//! important: CPU utilization is the difference between two observations, and the UI requests a
//! fresh observation every two seconds.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use sysinfo::{Pid, ProcessesToUpdate, System};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemPressure {
    /// Combined load score from 0 (completely idle) to 100 (maxed out).
    pub percent: u8,
    pub cpu_percent: u8,
    pub memory_percent: u8,
    pub cores: usize,
    pub label: String,
    /// Share of the whole machine (0-100) used by each AI's own CLI process and everything it started,
    /// keyed by agent id. AIs with no running process (idle, or API-based) are left out.
    #[serde(default)]
    pub agents: BTreeMap<String, u8>,
}

pub fn sample() -> SystemPressure {
    static HOST: OnceLock<Mutex<System>> = OnceLock::new();
    let host = HOST.get_or_init(|| Mutex::new(System::new_all()));
    let mut host = host.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    host.refresh_cpu_all();
    host.refresh_memory();
    host.refresh_processes(ProcessesToUpdate::All, true);

    let cpu_percent = bounded(host.global_cpu_usage() as f64);
    // On Apple Silicon this is the unified physical-memory pool. Available memory is preferable to
    // "used" memory because reclaimable caches should not make an idle machine look saturated.
    let memory_percent = if host.total_memory() == 0 {
        0
    } else {
        bounded(100.0 * (1.0 - host.available_memory() as f64 / host.total_memory() as f64))
    };
    // A fully saturated CPU or exhausted memory pool must read 100%, rather than being hidden by a
    // weighted average with the other resource.
    let percent = cpu_percent.max(memory_percent);

    let cores = host.cpus().len().max(1);
    let mut p = from_parts(percent, cpu_percent, memory_percent, cores);
    p.agents = per_agent(&host, cores);
    p
}

/// Which AI a CLI process belongs to, from its name or command line.
fn agent_of(command: &str) -> Option<&'static str> {
    let c = command.to_ascii_lowercase();
    let words = c.split(|ch: char| ch == '/' || ch == '\\' || ch.is_whitespace());
    for w in words {
        match w.trim_end_matches(".exe") {
            "codex" => return Some("gpt"),
            "claude" => return Some("claude"),
            "agy" => return Some("gemini"),
            _ => {}
        }
    }
    None
}

/// Sums CPU and memory of every process under each AI's CLI (a direct child of Consilium).
fn per_agent(host: &System, cores: usize) -> BTreeMap<String, u8> {
    let me = Pid::from_u32(std::process::id());
    let procs = host.processes();
    let mut owner: HashMap<Pid, &'static str> = HashMap::new();
    for (pid, p) in procs {
        if p.parent() == Some(me) {
            let mut cmd = p.name().to_string_lossy().into_owned();
            for part in p.cmd().iter().take(2) {
                cmd.push(' ');
                cmd.push_str(&part.to_string_lossy());
            }
            if let Some(a) = agent_of(&cmd) {
                owner.insert(*pid, a);
            }
        }
    }
    let total_mem = host.total_memory().max(1) as f64;
    let mut use_by: HashMap<&'static str, (f64, f64)> = HashMap::new();
    for (pid, p) in procs {
        // Walk up the parent chain until we reach an AI's CLI or run out (bounded against cycles).
        let mut cur = Some(*pid);
        let mut found = None;
        for _ in 0..32 {
            let Some(c) = cur else { break };
            if let Some(a) = owner.get(&c) {
                found = Some(*a);
                break;
            }
            cur = procs.get(&c).and_then(|x| x.parent());
        }
        if let Some(a) = found {
            let e = use_by.entry(a).or_default();
            e.0 += p.cpu_usage() as f64 / cores as f64;
            e.1 += 100.0 * p.memory() as f64 / total_mem;
        }
    }
    use_by.into_iter().map(|(a, (cpu, mem))| (a.to_string(), bounded(cpu.max(mem)))).collect()
}

fn bounded(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

fn from_parts(percent: u8, cpu_percent: u8, memory_percent: u8, cores: usize) -> SystemPressure {
    let label = match percent {
        90..=100 => "Saturated",
        70..=89 => "Heavy",
        40..=69 => "Moderate",
        _ => "Low",
    };
    SystemPressure { percent, cpu_percent, memory_percent, cores, label: label.to_string(), agents: BTreeMap::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_returns_reasonable_bounds() {
        let p = sample();
        assert!(p.percent <= 100);
        assert!(p.cpu_percent <= 100);
        assert!(p.memory_percent <= 100);
        assert!(p.cores >= 1);
        assert!(!p.label.is_empty());
    }

    #[test]
    fn the_busier_resource_sets_the_total_pressure() {
        assert_eq!(from_parts(100, 100, 12, 8).percent, 100);
        assert_eq!(from_parts(91, 20, 91, 8).label, "Saturated");
        assert_eq!(from_parts(22, 22, 10, 8).label, "Low");
    }

    #[test]
    fn cli_processes_map_to_their_agent() {
        assert_eq!(agent_of("codex exec --json"), Some("gpt"));
        assert_eq!(agent_of("/opt/node/bin/claude -p"), Some("claude"));
        assert_eq!(agent_of("node /opt/node/bin/claude -p"), Some("claude"));
        assert_eq!(agent_of("C:\\tools\\agy.exe --print"), Some("gemini"));
        assert_eq!(agent_of("cargo test"), None);
        assert_eq!(agent_of("claude-helper"), None);
    }

    #[test]
    fn percentages_are_bounded_and_rounded() {
        assert_eq!(bounded(-1.0), 0);
        assert_eq!(bounded(42.6), 43);
        assert_eq!(bounded(140.0), 100);
    }
}
