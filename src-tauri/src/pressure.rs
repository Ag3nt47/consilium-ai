//! Samples live host CPU and memory pressure for the 0-100% header gauge.
//!
//! `sysinfo` uses the native host APIs on macOS, Windows and Linux. Keeping one sampler alive is
//! important: CPU utilization is the difference between two observations, and the UI requests a
//! fresh observation every two seconds.

use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use sysinfo::System;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemPressure {
    /// Combined load score from 0 (completely idle) to 100 (maxed out).
    pub percent: u8,
    pub cpu_percent: u8,
    pub memory_percent: u8,
    pub cores: usize,
    pub label: String,
}

pub fn sample() -> SystemPressure {
    static HOST: OnceLock<Mutex<System>> = OnceLock::new();
    let host = HOST.get_or_init(|| Mutex::new(System::new_all()));
    let mut host = host.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    host.refresh_cpu_all();
    host.refresh_memory();

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

    from_parts(percent, cpu_percent, memory_percent, host.cpus().len().max(1))
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
    SystemPressure { percent, cpu_percent, memory_percent, cores, label: label.to_string() }
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
    fn percentages_are_bounded_and_rounded() {
        assert_eq!(bounded(-1.0), 0);
        assert_eq!(bounded(42.6), 43);
        assert_eq!(bounded(140.0), 100);
    }
}
