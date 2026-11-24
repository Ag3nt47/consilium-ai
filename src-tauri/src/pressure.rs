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
    /// Installed physical memory, rounded down to MiB. Exposed so the UI can explain why it
    /// selected a conservative profile without needing an OS-specific probe of its own.
    pub total_memory_mb: u64,
    pub available_memory_mb: u64,
    pub label: String,
    /// Baseline chosen from portable CPU/RAM facts: "conservative", "balanced" or "performance".
    pub hardware_profile: String,
    /// Runtime policy after current load is considered: "eco", "balanced" or "full".
    pub adaptive_mode: String,
    /// Maximum number of local AI CLI processes Consilium starts at once on this machine.
    pub recommended_parallel_agents: usize,
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
    let total_memory_mb = host.total_memory() / (1024 * 1024);
    let mut p = from_parts(percent, cpu_percent, memory_percent, cores, total_memory_mb);
    p.available_memory_mb = host.available_memory() / (1024 * 1024);
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

fn capacity(cores: usize, total_memory_mb: u64) -> (&'static str, usize) {
    // These deliberately err on the cautious side. Provider CLIs are browser-class processes;
    // starting three of them together on a 4-core/8-GiB computer can force the OS into swap.
    if cores <= 2 || total_memory_mb <= 4 * 1024 {
        ("conservative", 1)
    } else if cores <= 4 || total_memory_mb <= 8 * 1024 {
        ("balanced", 2)
    } else {
        ("performance", 3)
    }
}

/// Baseline process limit used by the engine on macOS, Windows and Linux. An environment override
/// is useful for managed workstations, but is bounded so a typo cannot create an unbounded fork.
pub fn parallel_agent_limit() -> usize {
    if let Ok(raw) = std::env::var("CONSILIUM_MAX_PARALLEL_AGENTS") {
        if let Ok(value) = raw.parse::<usize>() {
            return value.clamp(1, 8);
        }
    }
    let host = System::new_all();
    capacity(host.cpus().len().max(1), host.total_memory() / (1024 * 1024)).1
}

fn from_parts(percent: u8, cpu_percent: u8, memory_percent: u8, cores: usize, total_memory_mb: u64) -> SystemPressure {
    let label = match percent {
        90..=100 => "Saturated",
        70..=89 => "Heavy",
        40..=69 => "Moderate",
        _ => "Low",
    };
    let (hardware_profile, recommended_parallel_agents) = capacity(cores, total_memory_mb);
    let adaptive_mode = if percent >= 90 || hardware_profile == "conservative" {
        "eco"
    } else if percent >= 70 || hardware_profile == "balanced" {
        "balanced"
    } else {
        "full"
    };
    SystemPressure {
        percent,
        cpu_percent,
        memory_percent,
        cores,
        total_memory_mb,
        available_memory_mb: 0,
        label: label.to_string(),
        hardware_profile: hardware_profile.to_string(),
        adaptive_mode: adaptive_mode.to_string(),
        recommended_parallel_agents,
        agents: BTreeMap::new(),
    }
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
        assert!(p.total_memory_mb > 0);
        assert!((1..=8).contains(&p.recommended_parallel_agents));
        assert!(!p.label.is_empty());
    }

    #[test]
    fn the_busier_resource_sets_the_total_pressure() {
        assert_eq!(from_parts(100, 100, 12, 8, 16_384).percent, 100);
        assert_eq!(from_parts(91, 20, 91, 8, 16_384).label, "Saturated");
        assert_eq!(from_parts(22, 22, 10, 8, 16_384).label, "Low");
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

    #[test]
    fn hardware_capacity_is_portable_and_conservative() {
        assert_eq!(capacity(2, 32_768), ("conservative", 1));
        assert_eq!(capacity(16, 4_096), ("conservative", 1));
        assert_eq!(capacity(4, 8_192), ("balanced", 2));
        assert_eq!(capacity(8, 16_384), ("performance", 3));
        assert_eq!(from_parts(92, 92, 40, 12, 32_768).adaptive_mode, "eco");
        assert_eq!(from_parts(30, 30, 20, 12, 32_768).adaptive_mode, "full");
    }
}
