//! On-demand inventory of a local development environment, without launching provider tools.

use serde::Serialize;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DEPENDENCIES: usize = 128;
const MAX_MODEL_ENTRIES: usize = 1024;

#[derive(Serialize)]
pub(crate) struct EnvironmentAudit {
    platform: &'static str,
    architecture: &'static str,
    git_summary: Option<String>,
    project_set: bool,
    dependencies: Vec<Dependency>,
    dependency_note: Option<&'static str>,
    tools: Vec<(&'static str, bool)>,
    ollama_models: Vec<String>,
    ollama_note: Option<&'static str>,
}

#[derive(Serialize)]
struct Dependency {
    name: String,
    requested: String,
    locked: Option<String>,
    installed: Option<String>,
    lockfile_matches: Option<bool>,
}

pub(crate) fn sample(workspace: Option<&Path>) -> EnvironmentAudit {
    let (dependencies, dependency_note) = workspace.map_or_else(
        || {
            (
                Vec::new(),
                Some("Choose a project folder to audit its dependencies."),
            )
        },
        project_dependencies,
    );
    let manifest_root = std::env::var_os("OLLAMA_MODELS")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".ollama").join("models")));
    let (ollama_models, ollama_note) = manifest_root.map_or_else(
        || {
            (
                Vec::new(),
                Some("The local Ollama model folder could not be located."),
            )
        },
        |root| ollama_models(&root.join("manifests")),
    );
    EnvironmentAudit {
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        git_summary: workspace.and_then(crate::prompt::audit_git_summary),
        project_set: workspace.is_some(),
        dependencies,
        dependency_note,
        tools: crate::toolbox::audit_tools(),
        ollama_models,
        ollama_note,
    }
}

fn json_file(path: &Path) -> Option<Value> {
    // Only inspect regular files, including when the project intentionally symlinks files.
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut buffer = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut buffer)
        .ok()?;
    (buffer.len() as u64 <= MAX_MANIFEST_BYTES)
        .then(|| serde_json::from_slice(&buffer).ok())
        .flatten()
}

fn dependency_name(name: &str) -> bool {
    let segment = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    if let Some(scoped) = name.strip_prefix('@') {
        scoped
            .split_once('/')
            .is_some_and(|(scope, name)| segment(scope) && segment(name))
    } else {
        segment(name)
    }
}

fn project_dependencies(workspace: &Path) -> (Vec<Dependency>, Option<&'static str>) {
    let Some(manifest) = json_file(&workspace.join("package.json")) else {
        return (
            Vec::new(),
            Some("No readable npm package manifest in the selected project."),
        );
    };
    let lock = json_file(&workspace.join("package-lock.json"));
    let sections = ["dependencies", "devDependencies"];
    let mut entries = std::collections::BTreeMap::new();
    let mut truncated = false;
    for section in sections {
        for (name, requested) in manifest[section].as_object().into_iter().flatten() {
            let Some(requested) = requested.as_str() else {
                continue;
            };
            if !dependency_name(name) {
                continue;
            }
            if entries.len() >= MAX_DEPENDENCIES && !entries.contains_key(name) {
                truncated = true;
                continue;
            }
            entries.insert(name, (section, requested));
        }
    }
    let packages = lock.as_ref().and_then(|lock| lock["packages"].as_object());
    let dependencies = entries
        .into_iter()
        .map(|(name, (section, requested))| {
            let relative = format!("node_modules/{name}");
            let locked = packages
                .and_then(|packages| packages.get(&relative))
                .and_then(|entry| short_field(&entry["version"]));
            let installed = json_file(&workspace.join(&relative).join("package.json"))
                .and_then(|value| short_field(&value["version"]));
            let lockfile_matches = packages.map(|packages| {
                packages
                    .get("")
                    .and_then(|root| root[section][name].as_str())
                    == Some(requested)
                    && locked.is_some()
            });
            Dependency {
                name: name.clone(),
                requested: display_field(requested),
                locked,
                installed,
                lockfile_matches,
            }
        })
        .collect();
    let note = if truncated {
        Some("Some direct dependencies were omitted at the audit limit.")
    } else if packages.is_none() {
        Some("No supported npm lockfile was readable.")
    } else {
        None
    };
    (dependencies, note)
}

fn short_field(value: &Value) -> Option<String> {
    value.as_str().map(display_field)
}

fn display_field(text: &str) -> String {
    let redacted_url = reqwest::Url::parse(text).ok().and_then(|mut url| {
        url.password()?;
        url.set_password(None).ok()?;
        Some(url.to_string())
    });
    crate::redact::scrub(redacted_url.as_deref().unwrap_or(text))
        .chars()
        .take(160)
        .collect()
}

fn ollama_models(manifests: &Path) -> (Vec<String>, Option<&'static str>) {
    let mut models = Vec::new();
    let mut remaining = MAX_MODEL_ENTRIES;
    let result = scan_manifest_dir(manifests, &mut Vec::new(), &mut remaining, &mut models);
    models.sort();
    let note = match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound && remaining == MAX_MODEL_ENTRIES => {
            Some("No local Ollama manifest folder found.")
        }
        Err(_) => Some("The Ollama inventory could not be read completely."),
        Ok(()) if remaining == 0 => Some("Some Ollama models may be omitted at the audit limit."),
        Ok(()) => None,
    };
    (models, note)
}

fn scan_manifest_dir(
    dir: &Path,
    components: &mut Vec<String>,
    remaining: &mut usize,
    models: &mut Vec<String>,
) -> io::Result<()> {
    let mut children = fs::read_dir(dir)?;
    while *remaining > 0 {
        let Some(child) = children.next() else { break };
        let child = child?;
        *remaining -= 1;
        let kind = child.file_type()?;
        let name = child.file_name().to_string_lossy().into_owned();
        if kind.is_dir() && components.len() < 3 {
            components.push(name);
            let result = scan_manifest_dir(&child.path(), components, remaining, models);
            components.pop();
            result?;
        } else if kind.is_file() && components.len() == 3 {
            models.push(format!("{}:{name}", components.join("/")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_report_only_reads_direct_package_metadata() {
        let root = std::env::temp_dir().join(format!("consilium-env-{}", uuid::Uuid::new_v4()));
        let pkg = root.join("node_modules/@scope/example");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"dependencies":{"@scope/example":"^1","../outside":"1"}}"#,
        )
        .unwrap();
        fs::write(root.join("package-lock.json"), r#"{"packages":{"":{"dependencies":{"@scope/example":"^1"}},"node_modules/@scope/example":{"version":"1.2.3"}}}"#).unwrap();
        fs::write(
            pkg.join("package.json"),
            r#"{"version":"1.2.2","scripts":{"postinstall":"exit 1"}}"#,
        )
        .unwrap();
        let (dependencies, note) = project_dependencies(&root);
        assert!(note.is_none());
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].lockfile_matches, Some(true));
        assert_eq!(dependencies[0].locked.as_deref(), Some("1.2.3"));
        assert_eq!(dependencies[0].installed.as_deref(), Some("1.2.2"));
        assert_eq!(
            display_field("https://git:password-test@localhost/pkg"),
            "https://git@localhost/pkg"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ollama_inventory_lists_manifest_names_without_opening_weights() {
        let root = std::env::temp_dir().join(format!(
            "consilium-model-inventory-{}",
            uuid::Uuid::new_v4()
        ));
        let model = root.join("registry.local/library/qwen");
        fs::create_dir_all(&model).unwrap();
        fs::write(model.join("4b"), "manifest body is irrelevant to inventory").unwrap();
        fs::write(root.join("unrelated"), "not a manifest").unwrap();
        assert_eq!(
            ollama_models(&root),
            (vec!["registry.local/library/qwen:4b".into()], None)
        );
        let _ = fs::remove_dir_all(root);
    }
}
