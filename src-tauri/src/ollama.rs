//! Local Ollama model provider integration.
//!
//! Connects to an Ollama instance (default: `http://127.0.0.1:11434`), probes installed models
//! via `/api/tags`, and formats them for integration into Consilium's council.

use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OllamaTagModel {
    pub name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub details: Option<OllamaModelDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OllamaModelDetails {
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub parameter_size: String,
    #[serde(default)]
    pub quantization_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<OllamaTagModel>,
}

/// Normalizes and validates an Ollama endpoint URL.
/// Defaults to `http://127.0.0.1:11434` if not provided.
pub fn normalize_url(raw: Option<&str>) -> Result<String, String> {
    let url = raw.unwrap_or(DEFAULT_OLLAMA_URL).trim().trim_end_matches('/');
    if url.is_empty() {
        return Ok(DEFAULT_OLLAMA_URL.to_string());
    }
    // Leverage existing URL security validation from customai
    crate::customai::check_url(url)
}

/// Probes an Ollama daemon for installed models via its `/api/tags` endpoint.
pub async fn list_models(base_url: Option<&str>) -> Result<Vec<OllamaTagModel>, String> {
    let url = normalize_url(base_url)?;
    let endpoint = format!("{url}/api/tags");

    let client = reqwest::Client::builder()
        .user_agent("Consilium")
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .get(&endpoint)
        .send()
        .await
        .map_err(|e| format!("could not reach Ollama at {url}: {e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Ollama at {url} returned HTTP {status}: {body}"));
    }

    let parsed: TagsResponse = serde_json::from_str(&body)
        .map_err(|e| format!("invalid response from Ollama at {url}: {e}"))?;

    Ok(parsed.models)
}

/// Formats a clean human-readable name from a model tag (e.g. "llama3.2:latest" -> "Llama3.2").
pub fn clean_name(tag: &str) -> String {
    let base = tag.split(':').next().unwrap_or(tag).trim();
    if base.is_empty() {
        return "Ollama".to_string();
    }
    let mut chars = base.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Converts an Ollama tag model into a Consilium `CustomAi` descriptor.
pub fn to_custom_ai(model: &OllamaTagModel, base_url: Option<&str>) -> Result<crate::customai::CustomAi, String> {
    let raw_url = normalize_url(base_url)?;
    let open_ai_url = format!("{raw_url}/v1");
    let name = clean_name(&model.name);
    let id = crate::customai::slug(&name);

    Ok(crate::customai::CustomAi {
        id,
        name,
        base_url: open_ai_url,
        model: model.name.clone(),
        local: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_default_url() {
        assert_eq!(normalize_url(None).unwrap(), "http://127.0.0.1:11434");
        assert_eq!(normalize_url(Some("")).unwrap(), "http://127.0.0.1:11434");
        assert_eq!(normalize_url(Some("http://localhost:11434/")).unwrap(), "http://localhost:11434");
    }

    #[test]
    fn test_normalize_invalid_url() {
        assert!(normalize_url(Some("not-a-url")).is_err());
        assert!(normalize_url(Some("ftp://127.0.0.1:11434")).is_err());
    }

    #[test]
    fn test_parse_tags_response() {
        let sample = serde_json::json!({
            "models": [
                {
                    "name": "llama3.2:latest",
                    "model": "llama3.2:latest",
                    "size": 2019393189,
                    "digest": "a80c4f17acd5",
                    "details": {
                        "parent_model": "",
                        "format": "gguf",
                        "family": "llama",
                        "families": ["llama"],
                        "parameter_size": "3.2B",
                        "quantization_level": "Q4_K_M"
                    }
                },
                {
                    "name": "qwen2.5-coder:7b",
                    "model": "qwen2.5-coder:7b",
                    "size": 4684128000u64,
                    "digest": "b52ef0209d",
                    "details": null
                }
            ]
        });

        let tags: TagsResponse = serde_json::from_str(&sample.to_string()).expect("valid tags");
        assert_eq!(tags.models.len(), 2);
        assert_eq!(tags.models[0].name, "llama3.2:latest");
        assert_eq!(tags.models[0].size, 2019393189);
        assert_eq!(tags.models[1].name, "qwen2.5-coder:7b");
    }

    #[test]
    fn test_clean_name() {
        assert_eq!(clean_name("llama3.2:latest"), "Llama3.2");
        assert_eq!(clean_name("deepseek-coder-v2:16b"), "Deepseek-coder-v2");
        assert_eq!(clean_name("mistral"), "Mistral");
    }

    #[test]
    fn test_to_custom_ai() {
        let model = OllamaTagModel {
            name: "llama3.2:latest".to_string(),
            model: "llama3.2:latest".to_string(),
            size: 2019393189,
            digest: "a80c4f17acd5".to_string(),
            details: None,
        };

        let custom = to_custom_ai(&model, None).expect("valid custom ai");
        assert_eq!(custom.id, "llama32");
        assert_eq!(custom.name, "Llama3.2");
        assert_eq!(custom.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(custom.model, "llama3.2:latest");
        assert!(custom.local);
    }
}
