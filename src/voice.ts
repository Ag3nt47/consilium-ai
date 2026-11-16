// Voice chat: replies read aloud with the system's speech synthesis, and the human's speech
// transcribed with the WebKit speech recogniser when the window offers one.
import type { ChatEvent } from "./api";

/** Whether voice chat is on in a conversation: the latest "Voice chat is on/off" note from Consilium decides. */
export function voiceOn(events: ChatEvent[]): boolean {
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    if (e.sender !== "consilium" || e.kind !== "system") continue;
    if (e.text.startsWith("Voice chat is on")) return true;
    if (e.text.startsWith("Voice chat is off")) return false;
  }
  return false;
}

/** Text as it should be spoken: no code, markdown punctuation or long URLs. */
export function speakable(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, " (code shown in the chat) ")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/https?:\/\/\S+/g, "a link")
    .replace(/^\s*REQUEST:\s*/gim, "Request: ")
    .replace(/^\s*#+\s*/gm, "")
    .replace(/^\s*[-*•]\s+/gm, "")
    .replace(/\*\*|__|[*_~>|]/g, "")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

const synth: SpeechSynthesis | null = typeof window !== "undefined" && "speechSynthesis" in window ? window.speechSynthesis : null;
export const canSpeak = synth !== null;

// Each AI keeps the same voice; pitch differs too in case the system has only one voice.
const PITCH: Record<string, number> = { gpt: 1.0, claude: 0.9, gemini: 1.12 };
// Preferred system voices per AI, best first, so each sounds distinct without any setup.
const PREFERRED: Record<string, string[]> = {
  claude: ["Daniel", "Arthur", "Oliver", "Rishi", "Fred"],
  gpt: ["Samantha", "Ava", "Allison", "Susan", "Victoria"],
  gemini: ["Reed", "Evan", "Nathan", "Aaron", "Tom", "Alex"],
};
const VOICES_KEY = "consilium.voices";

/** The human's chosen voice per AI (voice name), or none for automatic. */
export function loadVoiceChoices(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(VOICES_KEY) || "{}");
  } catch {
    return {};
  }
}

export function saveVoiceChoice(agent: string, voiceName: string) {
  const all = loadVoiceChoices();
  if (voiceName) all[agent] = voiceName;
  else delete all[agent];
  try {
    localStorage.setItem(VOICES_KEY, JSON.stringify(all));
  } catch {
    /* applies until the app closes */
  }
}

/** Installed voices in the system language, for the Settings picker. */
export function availableVoices(): SpeechSynthesisVoice[] {
  const lang = (navigator.language || "en").slice(0, 2);
  return (synth?.getVoices() ?? []).filter((v) => v.lang.startsWith(lang));
}

export function voiceFor(agent: string): SpeechSynthesisVoice | null {
  const voices = availableVoices();
  if (!voices.length) return null;
  const chosen = loadVoiceChoices()[agent];
  const byName = (n: string) => voices.find((v) => v.name === n || v.name.startsWith(n + " "));
  if (chosen) {
    const v = byName(chosen);
    if (v) return v;
  }
  for (const n of PREFERRED[agent] ?? []) {
    const v = byName(n);
    if (v) return v;
  }
  const local = voices.filter((v) => v.localService);
  const pool = local.length ? local : voices;
  let h = 0;
  for (const c of agent) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return pool[h % pool.length];
}

/** Reads a short sample in an AI's voice, for the Settings picker. */
export function previewVoice(agent: string, name: string) {
  if (!synth) return;
  synth.cancel();
  const u = new SpeechSynthesisUtterance(`Hello, this is ${name}.`);
  const v = voiceFor(agent);
  if (v) u.voice = v;
  u.pitch = PITCH[agent] ?? 1;
  synth.speak(u);
}

export interface VoicePlaybackItem {
  agent: string;
  name: string;
}

export interface VoicePlaybackState {
  current: VoicePlaybackItem | null;
  queued: VoicePlaybackItem[];
}

/**
 * Reads messages aloud one after another. `onPlayback` identifies the AI that is speaking and the
 * replies waiting behind it, so voice chat can show an honest per-AI status instead of one generic
 * "an AI is speaking" animation.
 *
 * WebKit sometimes never fires `onend` (notably when the utterance object is garbage-collected or
 * speech is interrupted), which used to leave voice chat stuck on "Reading … aloud" with the
 * microphone off. So the current utterance is held strongly, a watchdog advances once the
 * synthesiser has gone quiet, and a generation counter ignores late or duplicate end events.
 */
export class Speaker {
  private queue: { agent: string; name: string; text: string }[] = [];
  private current: VoicePlaybackItem | null = null;
  private utterance: SpeechSynthesisUtterance | null = null;
  private gen = 0;
  private watchdog: number | undefined;
  constructor(private onPlayback: (state: VoicePlaybackState) => void) {}

  private report() {
    this.onPlayback({
      current: this.current,
      queued: this.queue.map(({ agent, name }) => ({ agent, name })),
    });
  }

  say(agent: string, name: string, text: string) {
    const t = speakable(text);
    if (!synth || !t) return;
    this.queue.push({ agent, name, text: `${name}: ${t}` });
    if (!this.current) this.next();
    else this.report();
  }

  private clearWatchdog() {
    window.clearInterval(this.watchdog);
    this.watchdog = undefined;
  }

  private next() {
    this.clearWatchdog();
    const gen = ++this.gen;
    const item = this.queue.shift();
    if (!synth || !item) {
      this.current = null;
      this.utterance = null;
      this.report();
      return;
    }
    this.current = { agent: item.agent, name: item.name };
    this.report();
    const u = new SpeechSynthesisUtterance(item.text);
    const v = voiceFor(item.agent);
    if (v) u.voice = v;
    u.pitch = PITCH[item.agent] ?? 1;
    const done = () => {
      if (gen === this.gen && this.utterance === u) this.next();
    };
    u.onend = u.onerror = done;
    this.utterance = u;
    synth.speak(u);
    // Advance if the synthesiser has been silent for two checks in a row without telling us.
    const started = Date.now();
    let quiet = 0;
    this.watchdog = window.setInterval(() => {
      if (gen !== this.gen) return this.clearWatchdog();
      const silent = !synth.speaking && !synth.pending;
      quiet = silent ? quiet + 1 : 0;
      if ((quiet >= 2 && Date.now() - started > 1000) || Date.now() - started > 10 * 60_000) done();
    }, 500);
  }

  /** Stops the reply being read now and moves on to the next one. */
  skip() {
    if (!this.current) return;
    this.gen++; // the cancelled utterance's end event must not advance twice
    synth?.cancel();
    this.next();
  }

  /** Stops speaking and drops everything queued. */
  stop() {
    this.clearWatchdog();
    this.gen++;
    this.queue = [];
    synth?.cancel();
    this.current = null;
    this.utterance = null;
    this.report();
  }
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Recognition = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const RecognitionCtor: (new () => Recognition) | undefined =
  typeof window !== "undefined" ? (window as any).SpeechRecognition ?? (window as any).webkitSpeechRecognition : undefined;
export const canListen = RecognitionCtor !== undefined;

/**
 * Listens continuously. Interim text goes to `onInterim`; once the human has been quiet for
 * `pauseMs` after speaking, the whole utterance goes to `onUtterance`.
 */
export class Listener {
  private rec: Recognition | null = null;
  private wanted = false;
  private final = "";
  private timer: number | undefined;

  constructor(
    private onInterim: (text: string) => void,
    private onUtterance: (text: string) => void,
    private onError: (message: string) => void,
    private pauseMs = 1500,
  ) {}

  start() {
    if (!RecognitionCtor || this.wanted) return;
    this.wanted = true;
    this.open();
  }

  private open() {
    if (!RecognitionCtor) return;
    const rec = new RecognitionCtor();
    rec.continuous = true;
    rec.interimResults = true;
    rec.lang = navigator.language || "en-US";
    rec.onresult = (ev: Recognition) => {
      let interim = "";
      for (let i = ev.resultIndex; i < ev.results.length; i++) {
        const r = ev.results[i];
        if (r.isFinal) this.final += r[0].transcript + " ";
        else interim += r[0].transcript;
      }
      this.onInterim((this.final + interim).trim());
      window.clearTimeout(this.timer);
      this.timer = window.setTimeout(() => this.flush(), this.pauseMs);
    };
    rec.onerror = (ev: Recognition) => {
      if (ev.error === "not-allowed" || ev.error === "service-not-allowed") {
        this.wanted = false;
        this.onError("Consilium isn't allowed to use the microphone or speech recognition. Allow it in System Settings → Privacy & Security, then switch voice chat on again.");
      }
    };
    // The recogniser stops by itself after a while; keep it going while voice chat wants it.
    rec.onend = () => {
      if (this.wanted && this.rec === rec) setTimeout(() => this.wanted && this.rec === rec && this.open(), 250);
    };
    this.rec = rec;
    try {
      rec.start();
    } catch {
      /* already started */
    }
  }

  private flush() {
    const text = this.final.trim();
    this.final = "";
    if (text) this.onUtterance(text);
  }

  /** Stops listening. Anything heard but not yet sent is dropped. */
  stop() {
    this.wanted = false;
    window.clearTimeout(this.timer);
    this.final = "";
    const rec = this.rec;
    this.rec = null;
    try {
      rec?.abort();
    } catch {
      /* not running */
    }
  }
}
