import { useEffect, useRef } from "react";
import type { AgentInfo, ChatEvent } from "./api";
import { COMPLETION_TTL_MS, isRecentCompletion, RainClock } from "./telemetry-rain.mjs";

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
type CompletionSample = { sample: BackdropSample; until: number; rate: number };
const inks = {
  local: ["#00F0FF", "rgba(0, 240, 255, .55)"],
  cloud: ["#A78BFA", "rgba(138, 92, 246, .55)"],
  mesh: ["#00FF9D", "rgba(0, 230, 140, .50)"],
  alert: ["#FFAA00", "rgba(255, 170, 0, .55)"],
  host: ["#A1BBCF", "rgba(161, 187, 207, .45)"],
};
// Decorative glyphs contain no source code, prompts or simulated telemetry.
const idle: BackdropSample = { kind: "host", text: "01ABCDEFGHIJKLMNOPQRSTUVWXYZ<>/{}[]=+" };

export default function TelemetryBackdrop({ prefs, agents, workers, events, meshPeers, memory, paused }: {
  prefs: BackdropPrefs; agents: AgentInfo[]; workers: string[]; events: ChatEvent[];
  meshPeers: number; memory?: number; paused: boolean;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const snapshot = useRef<BackdropSample[]>([]);
  const settings = useRef(prefs);
  const completion = useRef<CompletionSample | null>(null);
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
  useEffect(() => {
    completion.current = null;
    if (prefs.filter === "mesh") return;
    const now = Date.now();
    for (let index = events.length - 1, floor = Math.max(0, events.length - 300); index >= floor; index--) {
      const event = events[index];
      if (!isRecentCompletion(event.ts, now)) continue;
      const measured = event.usage?.output_rate_milli;
      if (event.kind !== "message" || event.via || measured === undefined || !Number.isFinite(measured) || measured <= 0) continue;
      const agent = agents.find((candidate) => candidate.id === event.sender);
      if (!agent || agent.local_model === undefined) continue;
      const rate = measured / 1000;
      completion.current = {
        sample: { kind: agent.local_model ? "local" : "cloud", text: `[COMPLETED] ${agent.name} ${rate.toFixed(1)} tok/s` },
        until: event.ts + COMPLETION_TTL_MS,
        rate,
      };
      break;
    }
  }, [agents, events, prefs.filter]);
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
    const clock = new RainClock();

    function resize() {
      width = element!.clientWidth;
      height = element!.clientHeight;
      const ratio = Math.min(devicePixelRatio || 1, 2, Math.sqrt(3_000_000 / Math.max(1, width * height)));
      element!.width = Math.max(1, Math.floor(width * ratio));
      element!.height = Math.max(1, Math.floor(height * ratio));
      brush!.setTransform(ratio, 0, 0, ratio, 0, 0);
      brush!.font = "12px ui-monospace, Consolas, monospace";
      positions = new Float32Array(Math.min(96, Math.floor(width / (cell * 2))));
      for (let column = 0; column < positions.length; column++) positions[column] = (column * 83) % Math.max(height, 1);
    }

    function draw(now: number) {
      frame = requestAnimationFrame(draw);
      const { fps, opacity, speed } = settings.current;
      const elapsed = clock.advance(now, fps);
      if (elapsed === null) return;
      const entries = snapshot.current;
      const lastCompletion = completion.current;
      const recentCompletion = lastCompletion && isRecentCompletion(lastCompletion.until - COMPLETION_TTL_MS, Date.now())
        ? lastCompletion : null;
      let active = 0;
      for (const sample of entries) if (sample.active) active++;
      const stride = active ? 1 : 2;
      brush!.clearRect(0, 0, width, height);
      for (let column = 0; column < positions.length; column++) {
        const measured = column === 0 ? recentCompletion : null;
        const velocity = 14 + (measured ? Math.min(measured.rate * .5, 90) : Math.min(active, 12) * 6);
        positions[column] = (positions[column] + elapsed * velocity * speed / 100) % (height + 90);
        if (column % stride) continue;
        const sample = measured?.sample ?? (entries.length ? entries[Math.floor(column / stride) % entries.length] : idle);
        const [head, trail] = inks[sample.kind];
        const row = Math.floor(positions[column] / cell);
        const x = column * cell * 2 + 8;
        const alpha = Math.min(.35, Math.max(.05, opacity / 100));
        const pulse = sample.kind === "alert" ? .78 + .22 * Math.sin(now / 450) : 1;
        const length = 10 + column % 5;
        for (let depth = length; depth >= 0; depth--) {
          const alphabet = depth ? idle.text : sample.text;
          const glyph = alphabet[(row + column * 7 + depth * 11) % alphabet.length];
          brush!.globalAlpha = alpha * pulse * (1 - depth / (length + 1));
          brush!.fillStyle = depth ? trail : head;
          brush!.fillText(glyph, x, (row - depth) * cell);
        }
      }
    }

    function sync() {
      cancelAnimationFrame(frame);
      clock.reset();
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
