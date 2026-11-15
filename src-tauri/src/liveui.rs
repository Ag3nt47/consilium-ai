//! Instant screen updates: a newer build of the window's page (`dist/`) applied without a restart.
//!
//! `scripts/stage-ui.mjs` builds the page and stages it in `<data>/ui/staged` with a `ui.json`
//! naming the engine it was built against (a fingerprint of `src-tauri`, see `build.rs`). The page
//! is served through `LiveAssets`, which reads from `<data>/ui/live` when that holds a page built
//! for this engine and from the copy built into the app otherwise. A staged page built against
//! another engine is never offered: that change needs Update Now's restart.
//!
//! A newly applied page has to call `ui_loaded` within `CONFIRM`; if it doesn't (a blank or broken
//! page), the previous live page is restored (or the built-in page is used when there wasn't one),
//! so a bad update can't strand the human.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tauri::utils::assets::{AssetKey, AssetsIter, CspHash};
use tauri::{Assets, Runtime};

/// The engine this binary was built from; a page staged against another one isn't compatible.
pub const BACKEND: &str = env!("CONSILIUM_BACKEND_FINGERPRINT");
/// The page built into this binary (see `build.rs`); a staged copy of it isn't offered.
pub const BUILT_IN_PAGE: &str = env!("CONSILIUM_PAGE_FINGERPRINT");
pub const CONFIRM: Duration = Duration::from_secs(20);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Manifest {
    pub version: String,
    pub backend: String,
    /// Fingerprint of the page's files; older manifests without it fall back to `version`.
    #[serde(default)]
    pub page: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct StagedUi {
    pub version: String,
}

fn ui_dir(data: &Path) -> PathBuf {
    data.join("ui")
}

fn manifest(dir: &Path) -> Option<Manifest> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("ui.json")).ok()?).ok()
}

/// A page in `dir` built for this engine.
fn compatible(dir: &Path) -> Option<Manifest> {
    manifest(dir).filter(|m| m.backend == BACKEND && dir.join("index.html").is_file())
}

/// Shared between the asset server and the commands.
#[derive(Default)]
pub struct Live {
    /// The folder pages are served from, or None for the built-in page.
    active: RwLock<Option<PathBuf>>,
    /// Set when a page from `active` was loaded and hasn't confirmed yet.
    pending: Mutex<Option<Instant>>,
}

impl Live {
    /// Picks up the live page from an earlier session, if it still matches this engine. A live
    /// page left over from before a full update is stale and removed. Records the engine's
    /// fingerprint so the staging script can tell whether a page needs a restart.
    pub fn open(data: &Path) -> Arc<Live> {
        let ui = ui_dir(data);
        let _ = std::fs::create_dir_all(&ui);
        let _ = std::fs::write(ui.join("backend.txt"), BACKEND);
        let live = ui.join("live");
        let old = ui.join("live.old");
        let active = match compatible(&live) {
            Some(_) => Some(live),
            None => {
                let _ = std::fs::remove_dir_all(&live);
                if compatible(&old).is_some() && std::fs::rename(&old, &live).is_ok() {
                    Some(live)
                } else {
                    let _ = std::fs::remove_dir_all(&old);
                    None
                }
            }
        };
        let pending = active.as_ref().map(|_| Instant::now());
        Arc::new(Live { active: RwLock::new(active), pending: Mutex::new(pending) })
    }

    pub fn version(&self) -> Option<String> {
        let dir = self.active.read().ok()?.clone()?;
        manifest(&dir).map(|m| m.version)
    }

    /// Fingerprint of the page being served: the live one's, or the built-in one's.
    fn page(&self) -> String {
        let dir = self.active.read().ok().and_then(|a| a.clone());
        match dir.and_then(|d| manifest(&d)) {
            Some(m) => m.page,
            None => BUILT_IN_PAGE.to_string(),
        }
    }

    /// The staged page, if it is built for this engine and its content differs from what's shown.
    pub fn staged(&self, data: &Path) -> Option<StagedUi> {
        let m = compatible(&ui_dir(data).join("staged"))?;
        let same = if m.page.is_empty() {
            self.version().as_deref() == Some(m.version.as_str())
        } else {
            m.page == self.page()
        };
        (!same).then_some(StagedUi { version: m.version })
    }

    /// Makes the staged page live. The window reloads itself afterwards and must confirm.
    pub fn apply(&self, data: &Path) -> Result<String, String> {
        let ui = ui_dir(data);
        let staged = ui.join("staged");
        let m = compatible(&staged).ok_or("no screen update for this version of Consilium is staged")?;
        let (live, old) = (ui.join("live"), ui.join("live.old"));
        let mut active = self.active.write().map_err(|_| "busy")?;
        let _ = std::fs::remove_dir_all(&old);
        if live.exists() {
            std::fs::rename(&live, &old).map_err(|e| format!("couldn't move the old page aside: {e}"))?;
        }
        if let Err(e) = std::fs::rename(&staged, &live) {
            let _ = std::fs::rename(&old, &live);
            return Err(format!("couldn't put the new page in place: {e}"));
        }
        *active = Some(live);
        *self.pending.lock().unwrap() = Some(Instant::now());
        Ok(m.version)
    }

    pub fn confirm(&self) {
        let Ok(active) = self.active.read() else { return };
        let Ok(mut pending) = self.pending.lock() else { return };
        *pending = None;
        if let Some(live) = active.as_ref() {
            if let Some(ui) = live.parent() {
                let _ = std::fs::remove_dir_all(ui.join("live.old"));
            }
        }
    }

    /// Restores the previous live page, or falls back to the built-in page, if the new one hasn't
    /// confirmed in time. True if it did.
    pub fn check(&self, data: &Path) -> bool {
        // `apply` takes these locks in the same order. Holding both prevents a late confirmation
        // from racing with rollback and deleting the page we're about to restore.
        let Ok(mut active) = self.active.write() else { return false };
        let Ok(mut pending) = self.pending.lock() else { return false };
        let expired = matches!(*pending, Some(t) if t.elapsed() > CONFIRM);
        if !expired {
            return false;
        }
        let ui = ui_dir(data);
        let live = ui.join("live");
        let old = ui.join("live.old");
        let _ = std::fs::remove_dir_all(&live);
        let restored = compatible(&old).is_some() && std::fs::rename(&old, &live).is_ok();
        if !restored {
            let _ = std::fs::remove_dir_all(&old);
        }
        *active = restored.then_some(live);
        *pending = None;
        true
    }
}

/// A relative asset path with no `..` or root parts, so a request can't leave the page's folder.
fn safe(key: &str) -> Option<PathBuf> {
    let p = Path::new(key.trim_start_matches('/'));
    p.components().all(|c| matches!(c, Component::Normal(_))).then(|| p.to_path_buf())
}

/// Reads a live asset only when its resolved path remains inside the live page. This blocks both
/// lexical traversal (`..`) and a symlink planted anywhere in the requested path.
fn read_live_asset(dir: &Path, key: &str) -> Option<Vec<u8>> {
    let root = std::fs::canonicalize(dir).ok()?;
    let asset = std::fs::canonicalize(root.join(safe(key)?)).ok()?;
    asset.starts_with(&root).then(|| std::fs::read(asset).ok()).flatten()
}

/// Serves the live page when there is one, the built-in page otherwise.
pub struct LiveAssets<R: Runtime> {
    pub built_in: Box<dyn Assets<R>>,
    pub live: Arc<Live>,
}

impl<R: Runtime> Assets<R> for LiveAssets<R> {
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        let dir = self.live.active.read().ok().and_then(|a| a.clone());
        match dir {
            // A live page is served alone: mixing in built-in files would mix two versions.
            Some(dir) => read_live_asset(&dir, key.as_ref()).map(Cow::Owned),
            None => self.built_in.get(key),
        }
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
        self.built_in.iter()
    }

    fn csp_hashes(&self, html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        // Vite pages have no inline scripts, so the built-in hashes (if any) are all that's needed.
        self.built_in.csp_hashes(html_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(dir: &Path, version: &str, backend: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("index.html"), version).unwrap();
        let m = Manifest { version: version.into(), backend: backend.into(), page: format!("p-{version}") };
        std::fs::write(dir.join("ui.json"), serde_json::to_string(&m).unwrap()).unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("consilium-liveui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// `cargo test print_fingerprint -- --ignored --nocapture`; must match
    /// `node scripts/backend-fingerprint.mjs`.
    #[test]
    #[ignore]
    fn print_fingerprint() {
        println!("fingerprint={BACKEND} page={BUILT_IN_PAGE}");
    }

    #[test]
    fn rejects_paths_that_leave_the_folder() {
        assert_eq!(safe("assets/index-1.js"), Some(PathBuf::from("assets/index-1.js")));
        assert_eq!(safe("/index.html"), Some(PathBuf::from("index.html")));
        assert_eq!(safe("../secret"), None);
        assert_eq!(safe("assets/../../x"), None);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_leave_the_live_page() {
        use std::os::unix::fs::symlink;

        let data = temp("symlink");
        let live = data.join("ui/live");
        std::fs::create_dir_all(live.join("assets")).unwrap();
        std::fs::write(live.join("inside.txt"), "inside").unwrap();
        std::fs::write(data.join("outside.txt"), "outside").unwrap();
        symlink(data.join("outside.txt"), live.join("assets/escape.txt")).unwrap();

        assert_eq!(read_live_asset(&live, "inside.txt"), Some(b"inside".to_vec()));
        assert_eq!(read_live_asset(&live, "assets/escape.txt"), None);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn offers_and_applies_only_pages_built_for_this_engine() {
        let data = temp("apply");
        let live = Live::open(&data);
        page(&data.join("ui/staged"), "a", "another-engine");
        assert_eq!(live.staged(&data), None);
        assert!(live.apply(&data).is_err());
        page(&data.join("ui/staged"), "b", BACKEND);
        assert_eq!(live.staged(&data), Some(StagedUi { version: "b".into() }));
        assert_eq!(live.apply(&data), Ok("b".into()));
        assert_eq!(live.version().as_deref(), Some("b"));
        assert!(!data.join("ui/staged").exists());
        assert_eq!(live.staged(&data), None);
        // A rebuild with identical content (new version label, same page) isn't offered.
        page(&data.join("ui/staged"), "b", BACKEND);
        let m = Manifest { version: "b2".into(), backend: BACKEND.into(), page: "p-b".into() };
        std::fs::write(data.join("ui/staged/ui.json"), serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(live.staged(&data), None);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_copy_of_the_built_in_page_is_not_offered() {
        let data = temp("builtin");
        let live = Live::open(&data);
        page(&data.join("ui/staged"), "e", BACKEND);
        let m = Manifest { version: "e".into(), backend: BACKEND.into(), page: BUILT_IN_PAGE.into() };
        std::fs::write(data.join("ui/staged/ui.json"), serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(live.staged(&data), None);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn an_unconfirmed_page_restores_the_previous_page_and_a_stale_one_is_dropped() {
        let data = temp("fallback");
        page(&data.join("ui/live"), "previous", BACKEND);
        page(&data.join("ui/staged"), "c", BACKEND);
        let live = Live::open(&data);
        live.apply(&data).unwrap();
        assert!(!live.check(&data));
        *live.pending.lock().unwrap() = Some(Instant::now() - CONFIRM - Duration::from_secs(1));
        assert!(live.check(&data));
        assert_eq!(live.version().as_deref(), Some("previous"));
        assert_eq!(std::fs::read_to_string(data.join("ui/live/index.html")).unwrap(), "previous");
        assert!(!data.join("ui/live.old").exists());

        page(&data.join("ui/staged"), "confirmed", BACKEND);
        live.apply(&data).unwrap();
        assert!(data.join("ui/live.old").exists());
        live.confirm();
        assert!(!data.join("ui/live.old").exists());

        page(&data.join("ui/live"), "d", "older-engine");
        assert_eq!(Live::open(&data).version(), None);
        assert!(!data.join("ui/live").exists());
        let _ = std::fs::remove_dir_all(&data);
    }
}
