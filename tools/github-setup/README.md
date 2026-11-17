# GitHub & Git Setup Tool for Consilium & AIs

A cross-platform helper tool for humans and AI council members to verify, authenticate, and connect GitHub repositories across **macOS**, **Windows**, and **Linux**.

---

## Why this exists

When AI agents (Claude, Codex, Gemini) run tasks that push, pull, or clone repositories, standard non-interactive command sessions cannot answer interactive password or 2FA prompts. 

This tool:
1. **Checks status without freezing**: Verifies Git, GitHub CLI (`gh`), authentication tokens, and credential helper status.
2. **Configures Git credentials once**: Runs `gh auth setup-git` so that `git push` and `git pull` automatically use GitHub CLI's authentication without prompting for credentials.
3. **Sets privacy commit identity**: Automatically configures `user.name` and privacy no-reply email (`<id>+<user>@users.noreply.github.com`) so personal email addresses are never exposed in commits.
4. **Enables easy repo cloning**: Clones public and private repositories directly into your project workspaces using authenticated credentials.

---

## Usage for AIs & Automated Scripts

### Check Status (JSON for machine parsing)
- **macOS / Linux**:
  ```bash
  ./tools/github-setup/github-setup.sh status --json
  ```
- **Windows (PowerShell)**:
  ```powershell
  .\tools\github-setup\GitHubSetup.ps1 Status -Json
  ```

### Configure Git Credentials & Identity
- **macOS / Linux**:
  ```bash
  ./tools/github-setup/github-setup.sh setup
  ```
- **Windows (PowerShell)**:
  ```powershell
  .\tools\github-setup\GitHubSetup.ps1 Setup
  ```

### Clone a Repository
- **macOS / Linux**:
  ```bash
  ./tools/github-setup/github-setup.sh clone owner/repo [optional-dest]
  ```
- **Windows (PowerShell)**:
  ```powershell
  .\tools\github-setup\GitHubSetup.ps1 Clone owner/repo [optional-dest]
  ```

---

## Interactive Browser Sign-in (for Humans)
If not yet authenticated:
- **macOS / Linux**:
  ```bash
  ./tools/github-setup/github-setup.sh login
  ```
- **Windows**:
  ```cmd
  tools\github-setup\github-setup.cmd Login
  ```
Or use **Settings → GitHub** directly in Consilium.
