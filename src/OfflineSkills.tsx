import rawSkills from "../docs/offline-skills.json?raw";

interface Skill {
  summary: string;
  command: string;
  guidance: string;
}

const skills: Record<string, Skill> = JSON.parse(rawSkills);

export default function OfflineSkills() {
  return (
    <details className="small">
      <summary>Offline skills</summary>
      <p className="muted small">Open a skill to see its instructions. Run the examples from a Consilium source checkout.</p>
      {Object.entries(skills).map(([id, skill]) => (
        <details key={id}>
          <summary>{skill.summary}</summary>
          <p>{skill.guidance}</p>
          <pre><code>{skill.command}</code></pre>
        </details>
      ))}
    </details>
  );
}
