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
function voiceFor(agent: string): SpeechSynthesisVoice | null {
  const lang = (navigator.language || "en").slice(0, 2);
  const voices = (synth?.getVoices() ?? []).filter((v) => v.lang.startsWith(lang) && v.localService);
  if (!voices.length) return null;
  let h = 0;
  for (const c of agent) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return voices[h % voices.length];
}

/** Reads messages aloud one after another. `onSpeaking` reports when speech starts and stops. */
export class Speaker {
  private queue: { agent: string; text: string }[] = [];
  private busy = false;
  constructor(private onSpeaking: (speaking: boolean) => void) {}

  say(agent: string, name: string, text: string) {
    const t = speakable(text);
    if (!synth || !t) return;
    this.queue.push({ agent, text: `${name}: ${t}` });
    if (!this.busy) this.next();
  }

  private next() {
    const item = this.queue.shift();
    if (!synth || !item) {
      if (this.busy) this.onSpeaking(false);
      this.busy = false;
      return;
    }
    if (!this.busy) this.onSpeaking(true);
    this.busy = true;
    const u = new SpeechSynthesisUtterance(item.text);
    const v = voiceFor(item.agent);
    if (v) u.voice = v;
    u.pitch = PITCH[item.agent] ?? 1;
    u.onend = u.onerror = () => this.next();
    synth.speak(u);
  }

  /** Stops speaking and drops everything queued. */
  stop() {
    this.queue = [];
    synth?.cancel();
    if (this.busy) this.onSpeaking(false);
    this.busy = false;
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
