<p align="center"><img src="assets/icon-1024.png" width="128" alt="Consilium icon: glowing neural network of interconnected AI nodes converging into a central cognitive nexus"></p>

<h1 align="center">Consilium - Local-First Multi-Agent AI Mesh</h1>
<p align="center"><i>A council of minds: the offline capable, peer-linked AI hive mind.</i></p>

Consilium is a desktop app and terminal client for putting several AI assistants to work in one conversation. Use **Ask** for a round of independent answers or **Discuss** to let the council compare approaches. Assign tasks on the shared board, inspect changes in Git, and review the requests that need your input.

[Download Consilium](https://github.com/Ag3nt47/consilium-ai/releases/latest) · [Six-document upgrade status](docs/UPGRADE_STATUS.md) · [Privacy](PRIVACY.md)

<p align="center"><img src="docs/media/demo.gif" width="860" alt="Consilium demo showing agents working through a shared task board and a review request"></p>

*The walkthrough uses the built-in demo workspace and invented conversation data.*

## What you can do

- Connect supported provider CLIs or an OpenAI-compatible endpoint. Use **Add → Add an AI** for local servers such as Ollama or LM Studio, with a compatible runner installed separately.
- Assign council members separate tasks. Use Git worktrees for isolated edits and review results before merging.
- Pair computers on a local network and sync shared conversations and task state. Windows and macOS have been exercised over wired Ethernet.
- Use the task board, token usage panel, and Settings to follow work. Development builds include per-document upgrade progress.

## Split inference experiment

A Windows PC and an Intel Mac generated tokens from the same split Qwen3 0.6B model and, in a separate run, a split Llama model over wired Ethernet. Those small-model runs demonstrate weight partitioning across two computers. They do not establish throughput or memory fit for a larger model. Runner and model compatibility, per-device memory, and network performance all matter.

Native Bluetooth transport, simultaneous transport use, phone workers, and automatic local fallback are tracked in the [upgrade ledger](docs/UPGRADE_STATUS.md). Check the ledger for the current verification state of each feature.

The [deployment vision](docs/DEPLOYMENT_VISION.md) describes how this could grow into a cross-platform build farm, offline research and publishing workspace, and daily operations console.

## Try the interface

| Council workspace | Add an AI |
| --- | --- |
| ![Consilium demo council with shared tasks](docs/media/council-walkthrough.png) | ![Consilium demo Add an AI screen](docs/media/add-ai-walkthrough.png) |

The default Netrunner theme, visionOS Glass, and Retro ’84 are available in Settings. To open the demo without starting the desktop app or a model runner, run `npm ci` and `npm run dev`, then open `http://127.0.0.1:1420/?step=5`. Use `?addai=1` to preview the AI setup form or `?settings=1` to preview Settings.

## Example projects

- **Park map:** Give one agent the map interface, another the trail data and attribution, and a third the offline acceptance test.
- **Book production:** Split a manuscript into developmental editing, copy editing, and export review, with the author approving changes and publication.
- **Home-lab maintenance:** Use a read-only inventory and sanitized logs to identify capacity constraints and recurring errors, then review a prioritized maintenance plan.

## CLI

```bash
consilium chat
consilium ask "Explain AST caching"
consilium ask "Review the design" --discuss 3
consilium ask "Review this file" --attach file.rs
consilium status
consilium list
```

## Getting Started

Download the installer for your operating system from the [latest release](https://github.com/Ag3nt47/consilium-ai/releases/latest). Development builds need Node.js, a stable Rust toolchain, and the Tauri system dependencies for your platform.

```bash
git clone https://github.com/Ag3nt47/consilium-ai.git
cd consilium-ai
npm ci
npm run app:dev
```

Use `npm run app:build` to package the desktop app. Run `npm test`, `npm run check:secrets`, and `cargo test --manifest-path src-tauri/Cargo.toml` to check changes.

## Data and permissions

Consilium stores conversation history locally. Linked peers can receive shared conversation data, and a provider receives the context needed for a turn you send to it. Use only local endpoints for work that must stay offline. Agent access and automatic command execution depend on your settings. See [Privacy](PRIVACY.md) and [Security](SECURITY.md).

## License

Consilium is open-source software licensed under the [MIT License](LICENSE).  
Copyright © 2026 Ag3nt47.
