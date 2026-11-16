import { useEffect, useState } from "react";
import { api, type CatalogTool } from "./api";

/** The exact message the Install button sends; the engine grants that one pinned install. */
export function catalogInstallText(tool: CatalogTool): string {
  return `Install from the tool catalog: ${tool.name}. Run this exact command once, check the tool works, and record it with a REMEMBER: line. INSTALL_COMMAND_JSON: ${JSON.stringify(tool.install_command)}`;
}

function askText(tool: CatalogTool): string {
  return `Please install ${tool.name} (${tool.source}) from the tool catalog. Find the official, pinned install command for this computer and ask me with an INSTALL_COMMAND_JSON request.`;
}

/**
 * Settings → Tools: tools Consilium knows about but doesn't ship. Nothing installs on its own:
 * Install sends the pinned command to one AI as the human's approval for that single install.
 */
export function ToolsSettings({
  ais,
  onSend,
}: {
  ais: { id: string; name: string }[];
  onSend: (text: string, to: string) => Promise<void>;
}) {
  const [tools, setTools] = useState<CatalogTool[] | null>(null);
  const [to, setTo] = useState(ais[0]?.id ?? "");
  const [note, setNote] = useState("");

  const reload = () => api.toolCatalog().then(setTools).catch((e) => setNote(String(e)));
  useEffect(() => {
    reload();
  }, []);
  useEffect(() => {
    if (!ais.some((a) => a.id === to)) setTo(ais[0]?.id ?? "");
  }, [ais, to]);

  const send = (text: string, name: string) =>
    onSend(text, to)
      .then(() => setNote(`Sent to ${ais.find((a) => a.id === to)?.name ?? to} about ${name}. Watch the chat for the result.`))
      .catch((e) => setNote(`Couldn't send it: ${e}`));

  return (
    <div className="tools-settings">
      <p className="muted small">
        Tools the AIs know about but Consilium doesn't download. <b>Install</b> sends the exact command shown to the AI
        you pick, as your approval for that one install. Admin rights still ask separately.
      </p>
      <label className="muted small">
        Install with{" "}
        <select value={to} onChange={(e) => setTo(e.target.value)} disabled={!ais.length}>
          {ais.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name}
            </option>
          ))}
        </select>{" "}
        <button className="link" title="Check again which tools are installed" onClick={reload}>
          Refresh
        </button>
      </label>
      {tools === null ? (
        <p className="muted small">Loading…</p>
      ) : (
        <ul className="tool-list">
          {tools.map((tool) => (
            <li key={tool.name}>
              <div className="tool-head">
                <b>{tool.name}</b>
                <span
                  className={`tool-state ${tool.installed ? "on" : ""}`}
                  title={tool.installed ?? (tool.detectable ? "Not found on this computer" : "Consilium can't tell whether this is installed")}
                >
                  {tool.installed ? "Installed" : tool.detectable ? "Not installed" : "Unknown"}
                </span>
                {tool.install_command ? (
                  <button
                    className="btn small-btn"
                    disabled={!to}
                    title={`Approve this one install and send it to the chosen AI: ${tool.install_command}`}
                    onClick={() => send(catalogInstallText(tool), tool.name)}
                  >
                    {tool.installed ? "Reinstall" : "Install"}
                  </button>
                ) : (
                  <button
                    className="btn ghost small-btn"
                    disabled={!to}
                    title="No pinned command for this computer yet: ask the chosen AI to find one and request it"
                    onClick={() => send(askText(tool), tool.name)}
                  >
                    Ask an AI
                  </button>
                )}
              </div>
              <div className="muted small">{tool.about}</div>
              {tool.install_command && <code className="tool-cmd">{tool.install_command}</code>}
            </li>
          ))}
        </ul>
      )}
      {note && <p className="muted small">{note}</p>}
    </div>
  );
}
