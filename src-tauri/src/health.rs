use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Dual-metric process health monitor.
pub struct ProcessHealth {
    pub pid: u32,
    pub last_activity: Instant,
    pub heartbeat_timeout: Duration,
    pub cpu_floor_percent: f32,
    system: System,
}

impl ProcessHealth {
    pub fn new(pid: u32, heartbeat_timeout: Duration) -> Self {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
        Self {
            pid,
            last_activity: Instant::now(),
            heartbeat_timeout,
            cpu_floor_percent: 1.0,
            system,
        }
    }

    pub fn record_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Evaluates whether the process is deadlocked vs actively reasoning.
    /// If stdout has been silent past the heartbeat timeout, checks if CPU activity is sustained.
    pub fn is_stalled(&mut self) -> bool {
        if self.last_activity.elapsed() < self.heartbeat_timeout {
            return false;
        }

        self.system.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(self.pid)]), true);
        if let Some(proc) = self.system.process(Pid::from_u32(self.pid)) {
            // If the process is consuming CPU (e.g. LLM local inference or AST analysis), it is not deadlocked
            proc.cpu_usage() < self.cpu_floor_percent
        } else {
            // Process died or exited
            true
        }
    }
}

/// Sniffs stream-json or stderr output for rate-limit / quota exhaustion triggers.
pub fn detect_rate_limit(output: &str) -> Option<Duration> {
    let lower = output.to_lowercase();
    if lower.contains("rate limit")
        || lower.contains("429")
        || lower.contains("resource_exhausted")
        || lower.contains("quota exceeded")
        || lower.contains("overloaded")
    {
        // Default to a 60-second cooldown bench if no exact reset time is provided
        Some(Duration::from_secs(60))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_rate_limit_patterns() {
        assert!(detect_rate_limit("Error: 429 Too Many Requests").is_some());
        assert!(detect_rate_limit("RESOURCE_EXHAUSTED: quota exceeded").is_some());
        assert!(detect_rate_limit("Model is currently overloaded").is_some());
        assert!(detect_rate_limit("{\"type\":\"content\",\"text\":\"All good\"}").is_none());
    }
}
