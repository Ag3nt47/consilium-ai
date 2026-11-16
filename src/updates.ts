// Checks GitHub Releases for a newer Consilium at most once a day (or when asked).
// It only notifies: nothing is downloaded or installed automatically. Offline or rate-limited
// checks fail quietly. Only the public releases API is contacted, with no identifying data.
import { getVersion } from "@tauri-apps/api/app";
import { api } from "./api";

export const REPO = "Ag3nt47/consilium-ai";
export const RELEASES_URL = `https://github.com/${REPO}/releases`;
const DAY_MS = 24 * 60 * 60 * 1000;
const KEY = "consilium.updates";

export interface UpdateInfo {
  current: string;
  latest: string;
  url: string;
  notes: string;
  publishedAt: string;
  /** True when the newer version is on GitHub's main branch but has no published installer yet. */
  unreleased?: boolean;
}

interface Cache {
  lastCheck: number;
  etag?: string;
  latest?: Omit<UpdateInfo, "current">;
}

function readCache(): Cache {
  try {
    return JSON.parse(localStorage.getItem(KEY) || "") as Cache;
  } catch {
    return { lastCheck: 0 };
  }
}

function writeCache(c: Cache) {
  try {
    localStorage.setItem(KEY, JSON.stringify(c));
  } catch {
    /* storage unavailable: just check again next time */
  }
}

/** Compares dotted versions like "1.2.10" and "v1.3.0". Returns >0 when a is newer. */
export function compareVersions(a: string, b: string): number {
  const parse = (v: string) => v.replace(/^v/i, "").split(/[.+-]/).slice(0, 3).map((n) => parseInt(n, 10) || 0);
  const [x, y] = [parse(a), parse(b)];
  for (let i = 0; i < 3; i++) if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) - (y[i] ?? 0);
  return 0;
}

/** Returns update details when a newer release exists, otherwise null. */
export async function checkForUpdate(force = false): Promise<UpdateInfo | null> {
  const current = await getVersion();
  const cache = readCache();
  const fresh = Date.now() - cache.lastCheck < DAY_MS;
  if (!force && fresh) {
    return cache.latest && compareVersions(cache.latest.latest, current) > 0 ? { current, ...cache.latest } : null;
  }
  try {
    const headers: Record<string, string> = { Accept: "application/vnd.github+json" };
    if (cache.etag && cache.latest) headers["If-None-Match"] = cache.etag;
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers });
    if (res.status === 304 && cache.latest) {
      writeCache({ ...cache, lastCheck: Date.now() });
    } else if (res.ok) {
      const r = await res.json();
      const latest = {
        latest: String(r.tag_name || ""),
        url: typeof r.html_url === "string" && r.html_url.startsWith(`https://github.com/${REPO}/`) ? r.html_url : RELEASES_URL,
        notes: String(r.body || "").slice(0, 4000),
        publishedAt: String(r.published_at || ""),
      };
      writeCache({ lastCheck: Date.now(), etag: res.headers.get("ETag") || undefined, latest });
      if (Array.isArray(r.assets) && compareVersions(latest.latest, current) > 0) {
        const asset = r.assets.find((a: Record<string, unknown>) => {
          const name = String(a.name || "").toLowerCase();
          if (navigator.userAgent.includes("Windows") || navigator.platform?.includes("Win")) {
            return name.endsWith("-setup.exe") || name.endsWith(".exe");
          }
          if (navigator.userAgent.includes("Linux") || navigator.platform?.includes("Linux")) {
            return name.endsWith(".appimage");
          }
          return false;
        });
        if (asset && typeof asset.browser_download_url === "string") {
          api.stageReleaseUpdate(latest.latest, asset.browser_download_url).catch(() => undefined);
        }
      }
    } else {
      // 404 (no releases yet), rate limits, etc.: remember we checked, try again tomorrow.
      writeCache({ ...cache, lastCheck: Date.now() });
    }
  } catch {
    return null; // offline: stay quiet
  }
  const c = readCache();
  if (c.latest && compareVersions(c.latest.latest, current) > 0) return { current, ...c.latest };
  // Releases can lag behind the source: report a newer version on main too, marked unreleased,
  // so "up to date" is never claimed while GitHub has something newer.
  if (!force) return null;
  try {
    const res = await fetch(`https://api.github.com/repos/${REPO}/contents/package.json?ref=main`, {
      headers: { Accept: "application/vnd.github.raw+json" },
    });
    if (!res.ok) return null;
    const main = String((await res.json()).version || "");
    if (compareVersions(main, current) <= 0) return null;
    return { current, latest: main, url: `https://github.com/${REPO}`, notes: "", publishedAt: "", unreleased: true };
  } catch {
    return null;
  }
}
