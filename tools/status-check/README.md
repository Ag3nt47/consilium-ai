# Connection Status Check Tool for Consilium

Cross-platform diagnostic tool that verifies the health and reachability of all Consilium connections:
- **Signal / Phone Messaging Bridge**: Checks daemon status (`signal-cli` JSON-RPC on port 7583), registered account, and council group routing.
- **Linked Computers (Multi-Computer Sync)**: Discovers registered peers (e.g. `MSI-AGENT47`), tests candidate routes (Ethernet LAN, Wi-Fi LAN, direct link-local `169.254.x.x`, Tailscale), checks active TCP sockets on port 8470, and audits sync outbox lag.
- **Council AI Agents & Local Models**: Checks CLI availability and credentials for Claude Code (`claude`), Gemini (`agy`), ChatGPT (`codex`), and local models (Ollama on port 11434, LM Studio on port 1234).
- **Network & GitHub Connectivity**: Tests internet gateway reachability (`github.com:443`) and GitHub Actions CI workflow status.

The diagnostic report is formatted with visual marks (`✓` working, `!` warning, `✗` error), posted directly into the newest active conversation in the Consilium chat, and sent straight to the human's Note to Self on Signal.

---

## Usage

### macOS & Linux (`status-check.sh`)
```bash
# Run full check and deliver to both chat and Signal
./tools/status-check/status-check.sh

# Dry run (print report to terminal without sending to chat or Signal)
./tools/status-check/status-check.sh --dry-run

# Output structured JSON report
./tools/status-check/status-check.sh --json

# Send only to Signal Note to Self
./tools/status-check/status-check.sh --signal

# Post only into Consilium chat
./tools/status-check/status-check.sh --chat
```

### Windows (`StatusCheck.ps1` / `status-check.cmd`)
```powershell
# Run full check and deliver to both chat and Signal
.\tools\status-check\StatusCheck.ps1

# Dry run
.\tools\status-check\StatusCheck.ps1 -DryRun

# Output JSON
.\tools\status-check\StatusCheck.ps1 -Json

# Or run via CMD
tools\status-check\status-check.cmd -DryRun
```

---

## App Startup & Restart Integration

Consilium includes dual integration for connection status monitoring:
1. **In-App Engine Service (`startup_check.rs`)**: Runs automatically inside Consilium upon every app launch and restart. It settles connections (up to 60 seconds), writes the report as a `[Consilium]` system note in the active conversation, and dispatches a single copy to Signal Note to Self.
2. **Standalone Script Suite (`tools/status-check/`)**: Can be executed manually, invoked by terminal scripts or git hooks, or hooked into system launch configurations (`launchd` on macOS or Task Scheduler on Windows) to verify connectivity before or during council workflows.
