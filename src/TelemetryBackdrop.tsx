import { useEffect, useRef } from "react";
import type { AgentInfo } from "./api";

export type BackdropMode = "rain" | "static" | "stealth";
export type BackdropPrefs = {
  enabled: boolean;
  mode: BackdropMode;
  filter: "all" | "mesh" | "ai";
  opacity: number;
  speed: number;
  fps: 15 | 30 | 60;
};

type BackdropSample = { kind: "local" | "cloud" | "mesh" | "alert" | "host"; text: string; active?: boolean };
const inks = {
  local: ["#00F0FF", "rgba(0, 240, 255, .22)"],
  cloud: ["#A78BFA", "rgba(138, 92, 246, .22)"],
  mesh: ["#00FF9D", "rgba(0, 230, 140, .20)"],
  alert: ["#FFAA00", "rgba(255, 170, 0, .35)"],
  host: ["#A1BBCF", "rgba(161, 187, 207, .15)"],
};
const idle: BackdropSample = { kind: "host", text: "· : ·" };

export default function TelemetryBackdrop({ prefs, agents, workers, meshPeers, memory, paused }: {
  prefs: BackdropPrefs; agents: AgentInfo[]; workers: string[]; meshPeers: number; memory?: number; paused: boolean;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const snapshot = useRef<BackdropSample[]>([]);
  const settings = useRef(prefs);
  useEffect(() => {
    const samples: BackdropSample[] = [];
    if (prefs.filter !== "mesh") {
      const now = Date.now();
      for (const agent of agents) {
        if (workers.includes(agent.id) && agent.local_model !== undefined) {
          const kind = agent.local_model ? "local" : "cloud";
          samples.push({ kind, text: `[${kind.toUpperCase()}] ${agent.name}`, active: true });
        }
        if (agent.member && (agent.benched_until ?? 0) > now) {
          samples.push({ kind: "alert", text: `[LIMIT] ${agent.name}` });
        }
      }
    }
    if (meshPeers && prefs.filter !== "ai") samples.push({ kind: "mesh", text: `[PEERLINK] ${meshPeers} authenticated` });
    if (memory !== undefined && prefs.filter === "all") samples.push({ kind: "host", text: `[HOST] RAM ${memory}%` });
    snapshot.current = samples;
  }, [agents, workers, meshPeers, memory, prefs.filter]);
  useEffect(() => { settings.current = prefs; }, [prefs]);

  const rain = prefs.enabled && prefs.mode === "rain" && !paused;
  useEffect(() => {
    if (!rain) return;
    const element = canvas.current;
    const brush = element?.getContext("2d");
    if (!element || !brush) return;
    const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
    const cell = 18;
    let width = 0;
    let height = 0;
    let positions = new Float32Array(0);
    let frame = 0;
    let last = 0;

    function resize() {
      width = element!.clientWidth;
      height = element!.clientHeight;
      const ratio = Math.min(devicePixelRatio || 1, 2, Math.sqrt(3_000_000 / Math.max(1, width * height)));
      element!.width = Math.ceil(width * ratio);
      element!.height = Math.ceil(height * ratio);
      brush!.setTransform(ratio, 0, 0, ratio, 0, 0);
      brush!.font = "12px ui-monospace, Consolas, monospace";
      positions = new Float32Array(Math.min(96, Math.floor(width / (cell * 2))));
      for (let column = 0; column < positions.length; column++) positions[column] = (column * 83) % Math.max(height, 1);
    }

    function draw(now: number) {
      frame = requestAnimationFrame(draw);
      const { fps, opacity, speed } = settings.current;
      if (now - last < 1000 / fps) return;
      const elapsed = last ? Math.min(now - last, 80) / 1000 : 0;
      last = now;
      const entries = snapshot.current;
      let active = 0;
      for (const sample of entries) if (sample.active) active++;
      const stride = active ? 2 : 5;
      brush!.clearRect(0, 0, width, height);
      for (let column = 0; column < positions.length; column++) {
        positions[column] = (positions[column] + elapsed * (14 + Math.min(active, 12) * 6) * speed / 100) % (height + 90);
        if (column % stride) continue;
        const sample = entries.length ? entries[Math.floor(column / stride) % entries.length] : idle;
        const [head, trail] = inks[sample.kind];
        const row = Math.floor(positions[column] / cell);
        const x = column * cell * 2 + 8;
        brush!.globalAlpha = Math.min(active || sample.kind === "alert" ? .35 : .13, opacity / 100);
        if (sample.kind === "alert") brush!.globalAlpha *= .78 + .22 * Math.sin(now / 450);
        for (let depth = 5; depth >= 0; depth--) {
          const glyph = sample.text[(row + column * 7 + depth) % sample.text.length];
          brush!.fillStyle = depth ? trail : head;
          brush!.fillText(glyph, x, (row - depth) * cell);
        }
      }
    }

    function sync() {
      cancelAnimationFrame(frame);
      last = 0;
      if (!document.hidden && !reducedMotion.matches) frame = requestAnimationFrame(draw);
      else brush!.clearRect(0, 0, width, height);
    }
    const observer = new ResizeObserver(resize);
    observer.observe(element);
    resize();
    sync();
    document.addEventListener("visibilitychange", sync);
    reducedMotion.addEventListener("change", sync);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      document.removeEventListener("visibilitychange", sync);
      reducedMotion.removeEventListener("change", sync);
    };
  }, [rain]);

  if (!rain) return <div className="telemetry-backdrop telemetry-grid" aria-hidden="true" />;
  return <canvas className="telemetry-backdrop" ref={canvas} aria-hidden="true" />;
}

export function BackdropSettings({ prefs, modes, update }: {
  prefs: BackdropPrefs; modes: Record<BackdropMode, string>; update: (change: Partial<BackdropPrefs>) => void;
}) {
  return (
    <fieldset className="telemetry-settings">
      <legend>Backdrop</legend>
      <label className="toggle">
        <input type="checkbox" checked={prefs.enabled} onChange={(event) => update({ enabled: event.target.checked })} />
        Enable telemetry backdrop
      </label>
      {prefs.enabled && (
        <>
          <label>Backdrop mode
            <select value={prefs.mode} onChange={(event) => update({ mode: event.target.value as BackdropMode })}>
              {Object.entries(modes).map(([id, label]) => <option key={id} value={id}>{label}</option>)}
            </select>
          </label>
          {prefs.mode === "rain" && (
            <>
              <label>Telemetry
                <select value={prefs.filter} onChange={(event) => update({ filter: event.target.value as BackdropPrefs["filter"] })}>
                  <option value="all">All telemetry</option><option value="mesh">Mesh only</option><option value="ai">AI usage only</option>
                </select>
              </label>
              <label>Rain opacity <input type="range" min="5" max="35" value={prefs.opacity} onChange={(event) => update({ opacity: event.target.valueAsNumber })} /> {prefs.opacity}%</label>
              <label>Velocity sensitivity <input type="range" min="0" max="200" value={prefs.speed} onChange={(event) => update({ speed: event.target.valueAsNumber })} /> {prefs.speed}%</label>
              <label>Frame rate
                <select value={prefs.fps} onChange={(event) => update({ fps: Number(event.target.value) as BackdropPrefs["fps"] })}>
                  <option value="15">15 FPS</option><option value="30">30 FPS</option><option value="60">60 FPS</option>
                </select>
              </label>
            </>
          )}
        </>
      )}
    </fieldset>
  );
}
