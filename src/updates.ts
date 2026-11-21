// Checks GitHub Releases for a newer Consilium every 30 minutes (or when asked). A newer release's
// installer is downloaded in the background and checked against GitHub's published SHA-256. The
// existing automatic installer applies it when enabled; Update Now remains available otherwise.
// Only the public releases API is contacted, with no identifying data.
import { getVersion } from "@tauri-apps/api/app";
import { OS } from "./platform";
import { api } from "./api";

export const REPO = "Ag3nt47/consilium-ai";
export const RELEASES_URL = `https://github.com/${REPO}/releases`;
// The caller runs every 30 minutes. Keep the freshness window shorter so timer jitter cannot make
// the next tick reuse a check that completed a few seconds after the previous tick started.
const CACHE_FRESH_MS = 25 * 60 * 1000;
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
  /** The installer for this computer, retained so a failed or interrupted staging can retry. */
  asset?: ReleaseAsset;
}

interface ReleaseAsset {
  name: string;
  url: string;
  digest: string;
}

let staging: Promise<void> | null = null;

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

function releaseAsset(assets: unknown[], linuxPackage: string | null): ReleaseAsset | undefined {
  const candidates = assets
    .filter((a): a is Record<string, unknown> => !!a && typeof a === "object")
    .map((a) => ({
      name: String(a.name || ""),
      url: typeof a.browser_download_url === "string" ? a.browser_download_url : "",
      digest: typeof a.digest === "string" ? a.digest : "",
    }))
    .filter((a) => a.url && a.digest);
  const matches = (a: ReleaseAsset) => {
    const name = a.name.toLowerCase();
    if (OS === "mac") return name.endsWith(".dmg");
    if (OS === "windows") return name.endsWith("-setup.exe");
    if (OS === "linux") return name.endsWith(linuxPackage ? `.${linuxPackage}` : ".appimage");
    return false;
  };
  const matching = candidates.filter(matches);
  if (OS === "mac") {
    return matching.find((a) => a.name.toLowerCase() === "consilium-macos.dmg") ?? matching[0];
  }
  return matching[0];
}

/** Stages a cached release asset once. A failed attempt is retried on the next scheduled check. */
function stageRelease(
  current: string,
  latest: Omit<UpdateInfo, "current"> | undefined,
  asset: ReleaseAsset | undefined,
  onStart?: (version: string) => void,
  onFailure?: (error: unknown) => void,
) {
  if (!latest || !asset || compareVersions(latest.latest, current) <= 0 || staging) return;
  staging = (async () => {
    const already = await api.updateAvailable().catch(() => null);
    if (already && compareVersions(already.version, latest.latest) >= 0) return;
    onStart?.(latest.latest);
    await api.stageReleaseUpdate(latest.latest, asset.url, asset.digest);
  })()
    .catch((error: unknown) => { onFailure?.(error); })
    .finally(() => {
      staging = null;
    });
}

/** Returns update details when a newer release exists, otherwise null. */
export async function checkForUpdate(
  force = false,
  onDownloadStart?: (version: string) => void,
  onDownloadFailure?: (error: unknown) => void,
): Promise<UpdateInfo | null> {
  const current = await getVersion();
  const cache = readCache();
  const fresh = Date.now() - cache.lastCheck < CACHE_FRESH_MS;
  if (!force && fresh) {
    stageRelease(current, cache.latest, cache.asset, onDownloadStart, onDownloadFailure);
    return cache.latest && compareVersions(cache.latest.latest, current) > 0 ? { current, ...cache.latest } : null;
  }
  try {
    const headers: Record<string, string> = { Accept: "application/vnd.github+json" };
    // Old caches lack the installer metadata. Fetch the full release once to fill it rather than
    // accepting a 304 that would leave this computer unable to stage the known update.
    if (cache.etag && cache.latest && cache.asset) headers["If-None-Match"] = cache.etag;
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
      const pkg = OS === "linux" ? await api.linuxPackageKind().catch(() => null) : null;
      const asset = Array.isArray(r.assets) ? releaseAsset(r.assets, pkg) : undefined;
      writeCache({ lastCheck: Date.now(), etag: res.headers.get("ETag") || undefined, latest, asset });
    } else {
      // 404 (no releases yet), rate limits, etc.: remember we checked, then try again on schedule.
      writeCache({ ...cache, lastCheck: Date.now() });
      if (force) throw new Error(`GitHub returned HTTP ${res.status}`);
    }
  } catch (error) {
    if (force) throw error;
    return null;
  }
  const c = readCache();
  stageRelease(current, c.latest, c.asset, onDownloadStart, onDownloadFailure);
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
