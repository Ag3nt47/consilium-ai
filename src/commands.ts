// Command boxes: fenced shell blocks in an AI's message get Run, Deny and Copy buttons.
// The block positions here must match `fenced_blocks` in src-tauri/src/runcmd.rs, because Run
// sends only the message id and the block's position; the engine reads the command itself.

export type Fence = { lang: string; code: string; start: number; end: number; closed: boolean };

/** Every fenced block in `text`, in order, with the line range it covers (end inclusive). */
export function fencedBlocks(text: string): Fence[] {
  const out: Fence[] = [];
  const lines = text.split("\n");
  let open: { lang: string; body: string[]; start: number } | null = null;
  lines.forEach((line, i) => {
    const t = line.trim();
    if (!open) {
      if (t.startsWith("```")) open = { lang: (t.slice(3).trim().split(/\s+/)[0] ?? "").toLowerCase(), body: [], start: i };
      return;
    }
    if (t === "```") {
      out.push({ lang: open.lang, code: open.body.join("\n"), start: open.start, end: i, closed: true });
      open = null;
    } else {
      open.body.push(line);
    }
  });
  if (open) {
    const o = open as { lang: string; body: string[]; start: number };
    out.push({ lang: o.lang, code: o.body.join("\n"), start: o.start, end: lines.length - 1, closed: false });
  }
  return out;
}

const SHELLS: Record<string, string> = {
  bash: "Terminal", sh: "Terminal", shell: "Terminal", console: "Terminal", terminal: "Terminal", "shell-session": "Terminal",
  zsh: "zsh",
  powershell: "PowerShell", pwsh: "PowerShell", ps1: "PowerShell", ps: "PowerShell",
  cmd: "Command Prompt", bat: "Command Prompt", batch: "Command Prompt", "bat-cmd": "Command Prompt",
};

/** The shell's display name when Run supports this language tag. */
export function shellName(lang: string): string | null {
  return SHELLS[lang] ?? null;
}

/** Whether a block would ask for admin rights (and so show the computer's own password prompt). */
export function needsAdmin(code: string): boolean {
  return /^\s*(\$\s+)?sudo\s/m.test(code) || /-Verb\s+RunAs/i.test(code);
}

/** The line tying the human's Ran/Denied message to one block. Must match `reference` in runcmd.rs. */
export function commandRef(eventId: string, index: number): string {
  return `(command ref ${eventId}#${index})`;
}

const REF = /\(command ref ([^\s#)]+)#(\d+)\)/g;

/** Blocks already run or denied, as "eventId#index" → "ran" | "denied". */
export function handledCommands(events: { sender: string; kind: string; text: string }[]): Map<string, "ran" | "denied"> {
  const out = new Map<string, "ran" | "denied">();
  for (const e of events) {
    if (e.sender !== "human" || e.kind !== "message") continue;
    for (const m of e.text.matchAll(REF)) out.set(`${m[1]}#${m[2]}`, /^\s*Denied command/i.test(e.text) ? "denied" : "ran");
  }
  return out;
}

/** The human's message when Deny is clicked on a command box. */
export function deniedText(who: string, eventId: string, index: number): string {
  return `Denied command from ${who}'s message: don't run it.\n${commandRef(eventId, index)}`;
}
