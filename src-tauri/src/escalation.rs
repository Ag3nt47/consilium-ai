use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::path::{Path, PathBuf};

/// Status of an escalated failure.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStatus {
    #[default]
    Pending,
    Resolved,
    Failed,
}

/// Structured record capturing a failure from a local/subordinate agent
/// and any subsequent mentor resolution.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EscalationRecord {
    pub id: String,
    pub timestamp_ms: u64,
    pub source_model: String,
    pub task_prompt: String,
    pub failed_code: String,
    pub error_output: String,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default)]
    pub mentor_model: Option<String>,
    #[serde(default)]
    pub resolved_code: Option<String>,
    #[serde(default)]
    pub status: EscalationStatus,
}

impl EscalationRecord {
    /// Creates a new pending escalation record with sanitized content.
    pub fn new(
        source_model: impl Into<String>,
        task_prompt: impl AsRef<str>,
        failed_code: impl AsRef<str>,
        error_output: impl AsRef<str>,
        attempt_count: u32,
    ) -> Self {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp_ms,
            source_model: source_model.into(),
            task_prompt: sanitize_secrets(task_prompt.as_ref()),
            failed_code: sanitize_secrets(failed_code.as_ref()),
            error_output: sanitize_secrets(error_output.as_ref()),
            attempt_count,
            mentor_model: None,
            resolved_code: None,
            status: EscalationStatus::Pending,
        }
    }

    /// Marks the escalation record as resolved by a mentor model.
    pub fn resolve(&mut self, mentor: impl Into<String>, resolved_code: impl AsRef<str>) {
        self.mentor_model = Some(mentor.into());
        self.resolved_code = Some(sanitize_secrets(resolved_code.as_ref()));
        self.status = EscalationStatus::Resolved;
    }
}

/// Redacts known secret patterns (API keys, bearer tokens, sensitive passwords)
/// to prevent token leakage in persistence logs.
pub fn sanitize_secrets(input: &str) -> String {
    let mut text = input.to_string();

    // Anthropic / OpenAI / Google API keys & Bearer tokens
    if let Ok(re) = regex::Regex::new(r"(?i)(sk-[a-zA-Z0-9_\-]{20,}|AIza[0-9A-Za-z-_]{35}|bearer\s+[a-zA-Z0-9_\-\.]{20,})") {
        text = re.replace_all(&text, "[REDACTED_SECRET]").to_string();
    }

    // Key/value pairs like api_key = "..." or password = "..."
    if let Ok(re) = regex::Regex::new(r#"(?i)(api[_-]?key|secret|password|access[_-]?token)\s*[:=]\s*["']?([^\s"'`]+)["']?"#) {
        text = re.replace_all(&text, "$1=[REDACTED]").to_string();
    }

    text
}

/// Returns the standard path to `escalation_learning.jsonl` within the given data folder.
pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("escalation_learning.jsonl")
}

/// Appends a structured escalation record as a JSON line to `logs/escalation_learning.jsonl`.
pub fn log_escalation(data_dir: &Path, record: &EscalationRecord) -> Result<(), String> {
    let path = log_path(data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create log dir: {e}"))?;
    }

    let serialized = serde_json::to_string(record)
        .map_err(|e| format!("failed to serialize escalation record: {e}"))?;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("failed to open {}: {e}", path.display()))?;

    IoWrite::write_all(&mut file, serialized.as_bytes())
        .and_then(|_| IoWrite::write_all(&mut file, b"\n"))
        .map_err(|e| format!("failed to write record to {}: {e}", path.display()))?;

    Ok(())
}

/// Reads up to `limit` escalation records from the JSONL log file, newest last.
pub fn read_escalations(data_dir: &Path, limit: usize) -> Result<Vec<EscalationRecord>, String> {
    let path = log_path(data_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(&path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    let reader = BufReader::new(file);

    let mut records = Vec::new();
    for line_res in reader.lines() {
        let line = line_res.map_err(|e| format!("failed to read line: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<EscalationRecord>(trimmed) {
            records.push(rec);
        }
    }

    if records.len() > limit {
        Ok(records.split_off(records.len() - limit))
    } else {
        Ok(records)
    }
}

/// Resolves an existing escalation record by ID with the mentor's solution.
/// Updates the record in place within `logs/escalation_learning.jsonl`.
pub fn resolve_escalation(
    data_dir: &Path,
    id: &str,
    mentor: &str,
    resolved_code: &str,
) -> Result<bool, String> {
    let path = log_path(data_dir);
    if !path.exists() {
        return Ok(false);
    }

    let mut all_records = read_escalations(data_dir, usize::MAX)?;
    let mut found = false;

    for rec in &mut all_records {
        if rec.id == id {
            rec.resolve(mentor, resolved_code);
            found = true;
            break;
        }
    }

    if !found {
        return Ok(false);
    }

    let temp_path = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temp_path)
            .map_err(|e| format!("failed to create temp escalation file: {e}"))?;
        for rec in &all_records {
            let serialized = serde_json::to_string(rec)
                .map_err(|e| format!("failed to serialize record: {e}"))?;
            IoWrite::write_all(&mut file, serialized.as_bytes())
                .and_then(|_| IoWrite::write_all(&mut file, b"\n"))
                .map_err(|e| format!("failed to write to temp file: {e}"))?;
        }
    }

    fs::rename(&temp_path, &path)
        .map_err(|e| format!("failed to replace escalation log with updated records: {e}"))?;

    Ok(true)
}

/// Formats resolved mentorship records into dynamic few-shot prompt context
/// for local/subordinate models to prevent repeating prior errors.
pub fn format_prompt_guidance(records: &[EscalationRecord], max_entries: usize) -> String {
    let resolved: Vec<&EscalationRecord> = records
        .iter()
        .filter(|r| r.status == EscalationStatus::Resolved && r.resolved_code.is_some())
        .collect();

    if resolved.is_empty() {
        return String::new();
    }

    let slice = if resolved.len() > max_entries {
        &resolved[resolved.len() - max_entries..]
    } else {
        &resolved[..]
    };

    let mut out = String::from("### Mentorship Error-Escalation Guidance (Past Self-Corrections)\n");
    out.push_str("Below are prior failure patterns and verified solutions. Do not repeat these errors:\n\n");

    for (idx, r) in slice.iter().enumerate() {
        let mentor = r.mentor_model.as_deref().unwrap_or("Mentor");
        let fix = r.resolved_code.as_deref().unwrap_or("");
        let _ = writeln!(
            out,
            "#### Example {}:\n- **Task Context**: {}\n- **Failed Attempt ({})**:\n```\n{}\n```\n- **Error Observed**:\n```\n{}\n```\n- **{} Verified Resolution**:\n```\n{}\n```\n",
            idx + 1,
            r.task_prompt.trim(),
            r.source_model,
            r.failed_code.trim(),
            r.error_output.trim(),
            mentor,
            fix.trim()
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_creation_and_sanitize() {
        let test_key = format!("sk-test-{}", "a".repeat(25));
        let prompt = format!("Task with {test_key}");
        let code = "def run(): api_key = '[REDACTED value]'";
        let test_bearer = format!("bearer {}", "b".repeat(25));
        let err = format!("Authentication error with {test_bearer}");

        let rec = EscalationRecord::new("llama3", prompt, code, err, 1);
        assert_eq!(rec.status, EscalationStatus::Pending);
        assert!(rec.task_prompt.contains("[REDACTED_SECRET]"));
        assert!(!rec.task_prompt.contains(&test_key));
        assert!(rec.failed_code.contains("api_key=[REDACTED]"));
        assert!(!rec.failed_code.contains("testpassword123"));
        assert!(rec.error_output.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn test_log_read_and_resolve() {
        let tmp = std::env::temp_dir().join(format!("consilium-esc-test-{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&tmp);

        let rec = EscalationRecord::new("llama3", "Sort a list", "def sort(l): return l.sort()", "TypeError: void", 1);
        let id = rec.id.clone();

        assert!(log_escalation(&tmp, &rec).is_ok());

        let loaded = read_escalations(&tmp, 10).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, id);
        assert_eq!(loaded[0].status, EscalationStatus::Pending);

        let resolved = resolve_escalation(&tmp, &id, "claude", "def sort(l): return sorted(l)").unwrap();
        assert!(resolved);

        let loaded2 = read_escalations(&tmp, 10).unwrap();
        assert_eq!(loaded2.len(), 1);
        assert_eq!(loaded2[0].status, EscalationStatus::Resolved);
        assert_eq!(loaded2[0].mentor_model.as_deref(), Some("claude"));
        assert_eq!(loaded2[0].resolved_code.as_deref(), Some("def sort(l): return sorted(l)"));

        let guidance = format_prompt_guidance(&loaded2, 5);
        assert!(guidance.contains("Mentorship Error-Escalation Guidance"));
        assert!(guidance.contains("def sort(l): return sorted(l)"));

        let _ = fs::remove_dir_all(&tmp);
    }
}