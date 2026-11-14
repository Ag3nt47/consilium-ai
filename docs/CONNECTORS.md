# Agent connectors

Every AI in Consilium is a **connector**. The turn-taking code only sees the interface below, so adding a new AI (Gemini, a local model, an API provider) means adding one connector, not changing the app.

## Interface

A connector provides:

| Operation | Returns | Rules |
| --- | --- | --- |
| `status()` | installed?, version, signed in?, path, setup hint | Must not read, print or send credentials. Use the provider's own status command. |
| `run(system, transcript, cancel)` | the reply text, or an error | Must honor `cancel` and the time limit by killing everything it started. No shell. No access to the person's files unless the person granted it for a project. |
| `setup(action)` | a fixed install or sign-in command | Shown to the person and run only after they confirm. |

In v0.1 these live in `src-tauri/src/agents.rs` (`status`, `run`) and `src-tauri/src/setup.rs` (`step`), with one `Kind` per connector.

## Manifest

Each connector is described by a manifest. v0.1 compiles the two built-in manifests in; v0.3 will load additional ones from the app's data folder.

```json
{
  "id": "gemini",
  "name": "Gemini",
  "transport": "local-cli",
  "tool": "Gemini CLI",
  "executable": { "names": ["gemini"], "extraPaths": [] },
  "status": { "version": ["--version"] },
  "run": {
    "systemPrompt": "stdin-prefix",
    "transcript": "stdin",
    "output": { "format": "json", "field": "response" }
  },
  "setup": {
    "install": { "macos": "npm install -g @google/gemini-cli", "linux": "npm install -g @google/gemini-cli", "windows": "npm install -g @google/gemini-cli" },
    "signin": "{exe}"
  },
  "capabilities": { "tools": false, "streaming": false, "files": false }
}
```

Field meanings:

- `transport`: `local-cli` (an official command-line tool that uses the person's own sign-in) or `api` (a provider API with a key stored in the OS keychain). The UI labels the two differently.
- `executable`: names to look for on `PATH`, plus extra install locations to check (apps started from the Dock don't get the shell's `PATH`).
- `run.systemPrompt`: `flag:<name>` passes Consilium's instructions as that flag's argument; `stdin-prefix` puts them before the transcript on standard input.
- `run.output`: `file` (the tool writes its reply to a file Consilium names), `json` with the field holding the reply, or `text`.
- `capabilities`: what the connector can do. Tools and file access stay off unless a project grants them (v0.2 Shared Projects).

The manifest is data, not code: it never contains commands built from user input, and `setup` commands are shown to the person before they run.

## Adding a connector

1. Write its manifest and confirm each command against the provider's current documentation.
2. Add a `Kind` with `status`, `run` and `setup` in `agents.rs` / `setup.rs` (until v0.3 loads manifests at run time).
3. Add tests with a fake executable, as in `agents.rs`: reply parsing, error reporting, arguments passed without a shell, Stop killing the process group, and the time limit.
4. Document it in the README's first-run table.
