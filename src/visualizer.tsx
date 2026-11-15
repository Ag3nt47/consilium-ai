// Voice visualizer: spectrum bars or a pulsing orb, cyan while the human speaks, violet while an AI speaks.
// The human's side uses real microphone levels when the window allows it (the microphone is never
// connected to the speakers). The system's read-aloud voice exposes no audio samples, so the AI side
// is a speaking-state animation rather than a measurement.
import { useEffect, useRef, useState } from "react";

export type VisualizerStyle = "bars" | "orb";
const KEY = "consilium.visualizer";
const BINS = 24;

export function savedVisualizerStyle(): VisualizerStyle {
  try {
    return localStorage.getItem(KEY) === "orb" ? "orb" : "bars";
  } catch {
    return "bars";
  }
}

/** Opens the microphone for levels only. Returns null if the window or the user doesn't allow it. */
async function openMic(): Promise<{ analyser: AnalyserNode; close: () => void } | null> {
  try {
    if (!navigator.mediaDevices?.getUserMedia) return null;
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const Ctx = window.AudioContext ?? (window as any).webkitAudioContext;
    const ctx: AudioContext = new Ctx();
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 64;
    analyser.smoothingTimeConstant = 0.8;
    ctx.createMediaStreamSource(stream).connect(analyser); // never to ctx.destination: no feedback
    return {
      analyser,
      close: () => {
        stream.getTracks().forEach((t) => t.stop());
        void ctx.close();
      },
    };
  } catch {
    return null;
  }
}

export function VoiceVisualizer({ speaker, active }: { speaker: "user" | "ai"; active: boolean }) {
  const [style, setStyle] = useState<VisualizerStyle>(savedVisualizerStyle);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const orbRef = useRef<HTMLDivElement | null>(null);
  const micRef = useRef<{ analyser: AnalyserNode; close: () => void } | null>(null);
  const [live, setLive] = useState(false);

  // The microphone is open only while it's the human's turn to speak.
  useEffect(() => {
    if (!active || speaker !== "user") return;
    let cancelled = false;
    void openMic().then((m) => {
      if (cancelled) return m?.close();
      micRef.current = m;
      setLive(m !== null);
    });
    return () => {
      cancelled = true;
      micRef.current?.close();
      micRef.current = null;
      setLive(false);
    };
  }, [active, speaker]);

  useEffect(() => {
    if (!active) return;
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    const data = new Uint8Array(BINS);
    const rgb = speaker === "user" ? "6, 182, 212" : "139, 92, 246";
    let id = 0;
    const draw = (t: number) => {
      const mic = micRef.current;
      if (mic) {
        const full = new Uint8Array(mic.analyser.frequencyBinCount);
        mic.analyser.getByteFrequencyData(full);
        for (let i = 0; i < BINS; i++) data[i] = full[Math.min(full.length - 1, i)];
      } else {
        // Speaking-state animation: smooth, speech-like motion, quieter for the idle microphone.
        const amp = speaker === "ai" ? 150 : 40;
        for (let i = 0; i < BINS; i++) {
          const v = Math.sin(t / 180 + i * 0.7) * 0.5 + Math.sin(t / 97 + i * 1.9) * 0.3 + Math.sin(t / 410) * 0.2;
          data[i] = Math.max(8, Math.min(255, amp * (0.55 + 0.45 * v) * (1 - i / (BINS * 1.6))));
        }
      }
      const canvas = canvasRef.current;
      if (style === "bars" && canvas) {
        const g = canvas.getContext("2d");
        if (g) {
          const w = canvas.width;
          const h = canvas.height;
          g.clearRect(0, 0, w, h);
          const bw = w / BINS - 2;
          const grad = g.createLinearGradient(0, h, 0, 0);
          grad.addColorStop(0, speaker === "user" ? "#06b6d4" : "#3b82f6");
          grad.addColorStop(1, speaker === "user" ? "#3b82f6" : "#8b5cf6");
          g.fillStyle = grad;
          for (let i = 0; i < BINS; i++) {
            const bh = Math.max(2, (data[i] / 255) * h);
            g.fillRect(i * (bw + 2), h - bh, bw, bh);
          }
        }
      }
      const orb = orbRef.current;
      if (style === "orb" && orb) {
        let sum = 0;
        for (let i = 0; i < BINS; i++) sum += data[i];
        const vol = sum / BINS;
        const glow = 8 + (vol / 128) * 22;
        orb.style.transform = `scale(${1 + (vol / 128) * 0.35})`;
        orb.style.boxShadow = `0 0 ${glow}px rgba(${rgb}, 0.8), inset 0 0 ${glow / 2}px rgba(${rgb}, 0.6)`;
      }
      if (!reduce) id = requestAnimationFrame(draw);
    };
    id = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(id);
  }, [active, speaker, style]);

  function toggle() {
    const next = style === "bars" ? "orb" : "bars";
    setStyle(next);
    try {
      localStorage.setItem(KEY, next);
    } catch {
      /* not saved; still switches for now */
    }
  }

  const who = speaker === "user" ? (live ? "your microphone level" : "listening") : "an AI speaking";
  return (
    <button
      type="button"
      className={`voice-viz ${style} ${speaker}`}
      onClick={toggle}
      title={`Shows ${who}. Click to switch to ${style === "bars" ? "the pulsing orb" : "spectrum bars"}.`}
      aria-label={`Voice visualizer (${style === "bars" ? "spectrum bars" : "pulsing orb"}); click to switch style`}
    >
      {style === "bars" ? <canvas ref={canvasRef} width={96} height={22} /> : <div ref={orbRef} className="voice-orb" />}
    </button>
  );
}
