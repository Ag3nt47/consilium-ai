//! System resources and capabilities shared across linked computers.
//!
//! Exposes host disk space, CPU load, memory utilization, and detected tools
//! so councils can make resource-aware build and task routing decisions.

use serde::{Deserialize, Serialize};
use std::path::Path;
use sysinfo::Disks;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostResources {
    pub computer: String,
    pub os: String,
    pub free_disk_bytes: u64,
    pub total_disk_bytes: u64,
    pub cpu_percent: u8,
    pub memory_percent: u8,
    #[serde(default)]
    pub total_memory_mb: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_memory_mb: Option<u64>,
    pub cores: usize,
    pub tools: Vec<String>,
    pub updated_at: u64,
}

impl HostResources {
    /// Samples local host resources: primary disk space, CPU & memory pressure, and installed tools.
    pub fn sample(computer: &str, target_path: Option<&Path>) -> Self {
        let os = std::env::consts::OS.to_string();
        let pressure = crate::pressure::sample();

        let (free_disk_bytes, total_disk_bytes) = sample_disk(target_path);

        let tools = crate::toolbox::inventory()
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        let updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Self {
            computer: computer.to_string(),
            os,
            free_disk_bytes,
            total_disk_bytes,
            cpu_percent: pressure.cpu_percent,
            memory_percent: pressure.memory_percent,
            total_memory_mb: pressure.total_memory_mb,
            available_memory_mb: Some(pressure.available_memory_mb),
            cores: pressure.cores,
            tools,
            updated_at,
        }
    }

    /// Formats the resources into a concise, readable summary.
    pub fn format_summary(&self) -> String {
        let free_gb = self.free_disk_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let total_gb = self.total_disk_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let tools_list = if self.tools.is_empty() {
            "none".to_string()
        } else {
            self.tools.join(", ")
        };
        format!(
            "{computer} ({os}): {free_gb:.1} GB free / {total_gb:.1} GB total, {cpu}% CPU, {mem}% RAM ({cores} cores), tools: [{tools_list}]",
            computer = self.computer,
            os = self.os,
            free_gb = free_gb,
            total_gb = total_gb,
            cpu = self.cpu_percent,
            mem = self.memory_percent,
            cores = self.cores,
            tools_list = tools_list,
        )
    }
}

/// Samples disk space for the volume containing `target_path`, or the root/system disk.
fn sample_disk(target_path: Option<&Path>) -> (u64, u64) {
    let disks = Disks::new_with_refreshed_list();
    if disks.is_empty() {
        return (0, 0);
    }

    if let Some(path) = target_path {
        if let Some(disk) = disks
            .iter()
            .filter(|d| path.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len())
        {
            return (disk.available_space(), disk.total_space());
        }
    }

    if let Some(disk) = disks.first() {
        return (disk.available_space(), disk.total_space());
    }

    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_host_resources() {
        let r = HostResources::sample("lukes-mbp", Some(Path::new("/tmp")));
        assert_eq!(r.computer, "lukes-mbp");
        assert!(!r.os.is_empty());
        assert!(r.total_memory_mb > 0);
        assert!(r.available_memory_mb.is_some_and(|available| available <= r.total_memory_mb));
        assert!(r.cores >= 1);
        let summary = r.format_summary();
        assert!(summary.contains("lukes-mbp"));
        assert!(summary.contains("GB free"));
        let mut old_report = serde_json::to_value(&r).unwrap();
        old_report.as_object_mut().unwrap().remove("total_memory_mb");
        old_report.as_object_mut().unwrap().remove("available_memory_mb");
        let old: HostResources = serde_json::from_value(old_report).unwrap();
        assert_eq!(old.total_memory_mb, 0);
        assert_eq!(old.available_memory_mb, None);
    }

    #[test]
    fn test_format_summary_custom() {
        let r = HostResources {
            computer: "msi-agent47".into(),
            os: "windows".into(),
            free_disk_bytes: 12 * 1024 * 1024 * 1024,
            total_disk_bytes: 512 * 1024 * 1024 * 1024,
            cpu_percent: 25,
            memory_percent: 45,
            total_memory_mb: 16384,
            available_memory_mb: Some(9000),
            cores: 16,
            tools: vec!["cargo".into(), "git".into(), "rea".into()],
            updated_at: 12345678,
        };
        let s = r.format_summary();
        assert_eq!(
            s,
            "msi-agent47 (windows): 12.0 GB free / 512.0 GB total, 25% CPU, 45% RAM (16 cores), tools: [cargo, git, rea]"
        );
    }
}
