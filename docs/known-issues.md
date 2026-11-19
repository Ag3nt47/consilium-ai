# Consilium known issues and fixes
Symptoms seen before, their causes, and the fix: check here before debugging Consilium itself.

This file ships with Consilium (`docs/known-issues.md` in the repo) and is copied into the skill
library on every start, so it is replaced on update. Add new findings to
`known-issues-notes.md` next to it, and propose lasting ones as a pull request to
`docs/known-issues.md`. Each entry: symptom, cause, fix, where in the code.

## Gemini / Antigravity
- **"Gemini tried to use a tool its access doesn't allow (Bash / ReadFile) and wrote no reply."**
  Cause: in print mode (`agy -p`), `--dangerously-skip-permissions` makes Antigravity refuse shell
  commands and reads outside the workspace (`denied_actions`). Fix (0.3.49): Unrestricted passes
  explicit `--settings` allow rules (`read_file(*)`, `write_file(*)`, `edit_file(*)`,
  `run_command(*)`) on every OS, and a turn that still ends on a refused tool is asked once more
  for a text answer. Code: `agents.rs` `antigravity_allow_rules`, `run_with_files_observed`.
  Also check Gemini's access level on *that* computer: the Mac had it on Edit, which has no shell.
- **Gemini's first answer is slow / Antigravity "restarts every prompt".** Print mode runs one
  prompt per process and startup takes about 3-4 s. Since 0.3.49 the next process is started
  ahead and waits for its prompt (`take_spare` / `prepare_spare` in `agents.rs`); set
  `CONSILIUM_NO_WARM_AGY=1` to turn that off when debugging.
- **`failed to decode stream input: invalid character '\ufeff'`** when testing `agy` by hand from
  PowerShell: PowerShell 5 writes a BOM to the child's stdin. Test from Python or `cmd /c ... < file`.
- Test Antigravity by hand (Windows, from a folder that is not the repo):
  `cmd /c "agy --input-format stream-json --output-format stream-json --print-timeout 0s --settings {...} --add-dir <dir> -p "" < msg.json"`
  with `msg.json` = `{"event":"user","message":{"content":"..."}}` written without a BOM.

## Local models (Ollama: Qwen, Llama, Phi, DeepSeek)
- **Slow first answer after a pause.** Ollama unloads a model after 5 idle minutes. Consilium keeps
  every local council member loaded while it is open (`ollama.rs` `keep_warm`, keep_alive 10m,
  renewed every 4 min). Check with `curl http://127.0.0.1:11434/api/ps`.
- **"empty reply"** from a local model: reasoning models put `<think>…</think>` in the reply.
  It is stripped (`apiagent.rs` `strip_think`); an empty answer is asked once more.
- **"connection refused" / Ollama not running.** Consilium runs `ollama serve` itself when the
  endpoint is this computer and nothing answers, then retries (`start_local_ollama`).
- Models on a data drive: set `OLLAMA_MODELS` (this PC: `D:\Ollama\models`) before `ollama pull`.
- A 7-8B model on a small GPU handles small, well-defined edits; give it focused tasks and point
  it at the runbooks in this library.

## Turns and auto-run
- **The chat stops: no AI answers, the human's messages are not even saved, the turn stays open
  with no AI process running.** An internal lock stayed taken (a freeze). Since 0.3.49 the freeze
  guard (`stall.rs`) restarts the app after about a minute and the turn resumes; see
  `stall.log` in the data folder. Until then: quit and reopen Consilium on each computer.
- **An Auto-run chat goes quiet after a command box.** Auto-continue held back while a box looked
  unrun and never looked again. Since 0.3.49 it rechecks every 30 s (`autocontinue.rs`
  `Decision::Recheck`). Auto-run rounds follow 5 s apart and pause after 25.
- **A command result arrived but its AI never answered it**: the result came in while that AI
  was mid-answer. Auto-continue (above) or any message picks it up.

## Data folder
- **Conversations vanish from the list.** A failed or racing save of `<id>.json` (title file).
  Since 0.3.47 saves go through `store::write_atomic` (unique temp file, flushed, retried
  rename, in-place fallback) and a missing title file is rebuilt from `<id>.jsonl`. Never write
  Consilium's data files from scripts while the app runs; use `consilium say <msg> -c <id>`.
- Windows: the data folder (`%APPDATA%\io.github.ag3nt47.consilium`) may be EFS-encrypted, where
  renames can fail with "not the same device"; that is why `replace_file` retries and falls back.

## Builds, releases, updates
- **Computers don't auto-update.** The release workflow publishes only when the version in
  `package.json` has no release yet: bump `package.json`, `src-tauri/tauri.conf.json`,
  `src-tauri/Cargo.toml`, the `consilium` entry in `Cargo.lock`, and `WHATS_NEW` in `lib.rs`.
- **MSI error 2753 / the old exe stays** when reinstalling the same version: bump the version.
- **CI fails with a clippy error you didn't see locally.** CI runs clippy with warnings as errors:
  run `cargo clippy --lib --tests -- -D warnings` in `src-tauri` before pushing.
- **Timing tests fail locally** (`everyone_answers_at_once…`, `pairs_with_code…`) while the PC is
  busy: they measure wall-clock time. Run `cargo test --lib -- --test-threads=4`, or the one test alone.
- **C: fills up during builds**: `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_INCREMENTAL=0`, and delete
  `src-tauri/target` when done.
- **Garbled text (`â†’` for `→`) or a whole file showing as changed**: a file was rewritten with
  PowerShell's `Set-Content -Encoding utf8` (BOM, cp1252 round trip) or with CRLF line endings.
  The repo is UTF-8 without BOM, LF. Write files with
  `[IO.File]::WriteAllText($p, $text, [Text.UTF8Encoding]::new($false))`, check `git diff --stat`.

## Working together
- Never delete a worktree, branch or folder you didn't create: another AI's uncommitted work was
  lost that way. `git worktree list` shows whose is whose (`ai/<agent>-<computer>/…` branches).
- Settings that differ per computer (access levels, council members, models) are per computer:
  fix them on the computer that has the problem.
