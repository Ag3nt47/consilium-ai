import { useState } from "react";
import { api } from "./api";

/** Common services and local model servers. Each only fills in the form; the person can change anything. */
const PRESETS: { label: string; name: string; url: string; local: boolean; model?: string; hint: string }[] = [
  { label: "Ollama (this computer)", name: "Llama", url: "http://localhost:11434/v1", local: true, model: "llama3.2", hint: "Install Ollama, run `ollama pull llama3.2`, and leave it running. No key needed." },
  { label: "LM Studio (this computer)", name: "LMStudio", url: "http://localhost:1234/v1", local: true, hint: "In LM Studio, load a model and start the local server. No key needed." },
  { label: "OpenRouter", name: "OpenRouter", url: "https://openrouter.ai/api/v1", local: false, hint: "One key for hundreds of models. Create it at openrouter.ai/keys." },
  { label: "Mistral", name: "Mistral", url: "https://api.mistral.ai/v1", local: false, model: "mistral-large-latest", hint: "Create a key at console.mistral.ai." },
  { label: "Groq", name: "Groq", url: "https://api.groq.com/openai/v1", local: false, hint: "Create a key at console.groq.com." },
  { label: "Together AI", name: "Together", url: "https://api.together.xyz/v1", local: false, hint: "Create a key at api.together.ai." },
  { label: "OpenAI API", name: "OpenAIAPI", url: "https://api.openai.com/v1", local: false, model: "gpt-4.1", hint: "Uses an OpenAI API key, billed separately from a ChatGPT plan." },
  { label: "Other (any OpenAI-compatible API)", name: "", url: "", local: false, hint: "Enter the service's API address; it usually ends in /v1." },
];

/** "Add your own AI": any OpenAI-compatible service with a key, or a model on this computer or home network. */
export function CustomAiForm({ onAdded, onError }: { onAdded: (id: string) => void; onError: (e: string) => void }) {
  const [preset, setPreset] = useState(-1);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [model, setModel] = useState("");
  const [local, setLocal] = useState(false);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);

  function pick(i: number) {
    setPreset(i);
    const p = PRESETS[i];
    if (!p) return;
    setName(p.name);
    setUrl(p.url);
    setModel(p.model ?? "");
    setLocal(p.local);
  }

  async function add() {
    setBusy(true);
    try {
      const id = await api.addCustomAi(name, url, model, local, local ? "" : key);
      setKey("");
      setPreset(-1);
      setName("");
      setUrl("");
      setModel("");
      onAdded(id);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="addai-card custom-ai-form">
      <div className="addai-head">
        <span className="agent-name">Add your own AI</span>
        <span className="muted small">a service with your own key, or a model on your computer</span>
      </div>
      <div className="custom-ai-presets">
        {PRESETS.map((p, i) => (
          <button key={p.label} title={p.hint} className={`btn ghost small-btn${preset === i ? " active" : ""}`} onClick={() => pick(i)}>
            {p.label}
          </button>
        ))}
      </div>
      {preset >= 0 && <p className="muted small">{PRESETS[preset].hint}</p>}
      <div className="custom-ai-fields">
        <label>
          Name <span className="muted small">(you'll @mention it by this)</span>
          <input value={name} maxLength={24} onChange={(e) => setName(e.target.value)} placeholder="Mistral" />
        </label>
        <label>
          API address
          <input value={url} spellCheck={false} onChange={(e) => setUrl(e.target.value)} placeholder="https://api.example.com/v1" />
        </label>
        <label>
          Model <span className="muted small">(optional: blank uses the first the service lists)</span>
          <input value={model} spellCheck={false} onChange={(e) => setModel(e.target.value)} placeholder="model-name" />
        </label>
        <label className="row">
          <input type="checkbox" checked={local} onChange={(e) => setLocal(e.target.checked)} />
          Runs on this computer or my home network (no key)
        </label>
        {!local && (
          <label>
            API key <span className="muted small">(saved only on this computer, never in the chat)</span>
            <input type="password" autoComplete="off" spellCheck={false} value={key} onChange={(e) => setKey(e.target.value)} placeholder="API key" />
          </label>
        )}
      </div>
      <div className="row end">
        <button
          title="Save this AI on this computer; then add it to the council from the list above"
          className="btn primary small-btn"
          disabled={busy || !name.trim() || !url.trim() || (!local && !key.trim())}
          onClick={add}
        >
          {busy ? "Adding…" : "Add AI"}
        </button>
      </div>
    </div>
  );
}
