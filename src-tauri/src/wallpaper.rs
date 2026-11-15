//! Bing Daily Wallpaper fetcher and local caching service.
//!
//! Fetches today's Bing daily wallpaper metadata asynchronously, downloads and caches the image
//! in `~/.config/consilium/wallpapers`, and returns it as a base64 data URL with a frosted glass overlay
//! for the messaging and chat container.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const BING_API_URL: &str =
    "https://www.bing.com/HPImageArchive.aspx?format=js&idx=0&n=1&mkt=en-US";
pub const BING_BASE_URL: &str = "https://www.bing.com";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DailyWallpaper {
    pub data_url: String,
    pub title: Option<String>,
    pub copyright: Option<String>,
    pub date: String,
}

#[derive(Debug, Deserialize)]
pub struct BingApiResponse {
    #[serde(default)]
    pub images: Vec<BingImageInfo>,
}

#[derive(Debug, Deserialize)]
pub struct BingImageInfo {
    #[serde(default)]
    pub startdate: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub copyright: Option<String>,
}

/// Standard location for cached Bing wallpapers: `~/.config/consilium/wallpapers`.
pub fn wallpaper_dir() -> PathBuf {
    let base = dirs::home_dir()
        .map(|h| h.join(".config").join("consilium"))
        .or_else(|| dirs::config_dir().map(|c| c.join("consilium")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("wallpapers")
}

/// Fast, dependency-free standard base64 encoder.
pub fn to_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Fetches today's Bing wallpaper metadata, downloads the image if missing, and caches it.
/// Prunes older `bing_*.jpg` files from the wallpaper directory to conserve disk space.
pub async fn fetch_and_cache_daily_wallpaper(
    client: &reqwest::Client,
    dir: &Path,
) -> Result<DailyWallpaper, String> {
    let _ = std::fs::create_dir_all(dir);

    let resp = client
        .get(BING_API_URL)
        .header(reqwest::header::USER_AGENT, "ConsiliumDesktopApp/1.0")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("Bing API request failed: {e}"))?;

    if !resp.status().is_success() {
        return get_fallback_wallpaper(dir)
            .ok_or_else(|| format!("Bing API returned status {}", resp.status()));
    }

    let bing_data = resp
        .json::<BingApiResponse>()
        .await
        .map_err(|e| format!("failed to parse Bing API response: {e}"))?;

    let info = bing_data
        .images
        .first()
        .ok_or_else(|| "no images found in Bing response".to_string())?;

    let start_date = if info.startdate.trim().is_empty() {
        "today".to_string()
    } else {
        info.startdate.trim().to_string()
    };

    let target_file = dir.join(format!("bing_{start_date}.jpg"));

    if !target_file.exists() {
        // Remove older cached bing_*.jpg files to save disk space
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("bing_") && name.ends_with(".jpg") && p != target_file {
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
        }

        let img_url = if info.url.starts_with("http://") || info.url.starts_with("https://") {
            info.url.clone()
        } else {
            format!("{BING_BASE_URL}{}", info.url)
        };

        let img_resp = client
            .get(&img_url)
            .header(reqwest::header::USER_AGENT, "ConsiliumDesktopApp/1.0")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("failed to download Bing wallpaper image: {e}"))?;

        if !img_resp.status().is_success() {
            return get_fallback_wallpaper(dir)
                .ok_or_else(|| format!("failed to download image: HTTP {}", img_resp.status()));
        }

        let bytes = img_resp
            .bytes()
            .await
            .map_err(|e| format!("failed to read image payload: {e}"))?;

        let _ = std::fs::write(&target_file, &bytes);

        let data_url = format!("data:image/jpeg;base64,{}", to_base64(&bytes));
        return Ok(DailyWallpaper {
            data_url,
            title: info.title.clone(),
            copyright: info.copyright.clone(),
            date: start_date,
        });
    }

    let bytes = std::fs::read(&target_file).map_err(|e| {
        format!(
            "failed to read cached wallpaper {}: {e}",
            target_file.display()
        )
    })?;
    let data_url = format!("data:image/jpeg;base64,{}", to_base64(&bytes));
    Ok(DailyWallpaper {
        data_url,
        title: info.title.clone(),
        copyright: info.copyright.clone(),
        date: start_date,
    })
}

/// Fallback routine returning the newest cached `bing_*.jpg` image if offline or Bing API unreachable.
pub fn get_fallback_wallpaper(dir: &Path) -> Option<DailyWallpaper> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("bing_") && name.ends_with(".jpg") {
                    files.push(p);
                }
            }
        }
    }
    files.sort();
    let latest = files.pop()?;
    let bytes = std::fs::read(&latest).ok()?;
    let name = latest.file_name()?.to_str()?;
    let date = name
        .strip_prefix("bing_")?
        .strip_suffix(".jpg")?
        .to_string();
    let data_url = format!("data:image/jpeg;base64,{}", to_base64(&bytes));
    Some(DailyWallpaper {
        data_url,
        title: None,
        copyright: None,
        date,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_base64_encodes_correctly() {
        assert_eq!(to_base64(b""), "");
        assert_eq!(to_base64(b"f"), "Zg==");
        assert_eq!(to_base64(b"fo"), "Zm8=");
        assert_eq!(to_base64(b"foo"), "Zm9v");
        assert_eq!(to_base64(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn fallback_picks_latest_cached_file() {
        let tmp = std::env::temp_dir().join(format!("consilium-wp-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        std::fs::write(tmp.join("bing_20261110.jpg"), b"older").unwrap();
        std::fs::write(tmp.join("bing_20261114.jpg"), b"newer").unwrap();
        std::fs::write(tmp.join("other.txt"), b"ignored").unwrap();

        let fallback = get_fallback_wallpaper(&tmp).expect("should find fallback");
        assert_eq!(fallback.date, "20261114");
        assert_eq!(
            fallback.data_url,
            format!("data:image/jpeg;base64,{}", to_base64(b"newer"))
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bing_response_deserialization() {
        let json = r#"{
            "images": [
                {
                    "startdate": "20261115",
                    "url": "/th?id=OHR.Test_1920x1080.jpg",
                    "title": "Test Title",
                    "copyright": "Test Copyright"
                }
            ]
        }"#;
        let parsed: BingApiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.images.len(), 1);
        let img = &parsed.images[0];
        assert_eq!(img.startdate, "20261115");
        assert_eq!(img.title.as_deref(), Some("Test Title"));
    }
}
