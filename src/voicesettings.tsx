// Settings: which system voice reads each AI's replies aloud in voice chat.
import { useEffect, useState } from "react";
import { availableVoices, canSpeak, loadVoiceChoices, previewVoice, saveVoiceChoice, voiceFor } from "./voice";

export function VoiceSettings({ agents }: { agents: { id: string; name: string }[] }) {
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>(() => availableVoices());
  const [choices, setChoices] = useState<Record<string, string>>(loadVoiceChoices);
  useEffect(() => {
    if (!canSpeak) return;
    const load = () => setVoices(availableVoices());
    load();
    window.speechSynthesis.addEventListener("voiceschanged", load);
    return () => window.speechSynthesis.removeEventListener("voiceschanged", load);
  }, []);
  if (!canSpeak) return <p className="muted small">Read-aloud isn't available on this system.</p>;
  return (
    <>
      <p className="muted small">
        The voice that reads each AI's replies in voice chat. Automatic picks a distinct installed voice for each. More
        natural voices can be added in System Settings → Accessibility → Spoken Content → System Voice → Manage Voices
        (Windows: Settings → Time &amp; language → Speech).
      </p>
      {agents.map((a) => (
        <div className="row" key={a.id}>
          <span style={{ minWidth: "6em" }}>{a.name}</span>
          <select
            title={`Voice used to read ${a.name}'s replies aloud`}
            value={choices[a.id] ?? ""}
            onChange={(e) => {
              saveVoiceChoice(a.id, e.target.value);
              setChoices(loadVoiceChoices());
            }}
          >
            <option value="">Automatic ({voiceFor(a.id)?.name ?? "system default"})</option>
            {voices.map((v) => (
              <option key={v.voiceURI} value={v.name}>
                {v.name}
                {v.localService ? "" : " (online)"}
              </option>
            ))}
          </select>
          <button title={`Hear a sample of ${a.name}'s voice`} className="btn ghost small" onClick={() => previewVoice(a.id, a.name)}>
            Preview
          </button>
        </div>
      ))}
    </>
  );
}
