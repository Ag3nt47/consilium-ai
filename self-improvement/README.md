# Self-improvement

How Consilium's AIs (and people) find, log and fix problems in Consilium itself, without burning tokens and without the AIs changing the app on their own.

## The loop

1. **Check cheaply.** `node self-improvement/check.mjs` runs local, free checks only: the privacy gate, `npm audit`, and, when the GitHub CLI is signed in, the repository's open Dependabot, code-scanning and secret-scanning alerts. **It never calls an AI.**
2. **Log, deduplicated.** New findings are added to [`ledger.json`](ledger.json) with an id, severity, source, evidence and date. A finding already in the ledger is not added again. If nothing new turned up, the script says so and stops, and nobody spends a token.
3. **Patch only when there's something to fix.** Any participant (ChatGPT, Claude, or a person) can pick an open ledger entry and propose a fix on a branch named `fix/<ledger-id>`, as a pull request that references the entry.
4. **Both agree.** The other AI reviews the pull request. A fix needs:
   - approval from both AIs, recorded as review comments starting `APPROVE <ledger-id>`;
   - every CI check green (tests on three platforms, privacy gate, CodeQL);
   - no new findings from `check.mjs`.
5. **Merge, release, close.** A maintainer merges to `main`, which can be the human or an AI the human has explicitly allowed to merge. The entry is marked `fixed` with the pull request number. Security fixes go out in the next patch release; the app's daily update check tells users.

Nothing in this folder modifies code, merges, or publishes by itself; those steps always go through a pull request and CI.

## When to run it

- In CI on a weekly schedule (`.github/workflows/self-check.yml`), which uploads the report; new findings fail the job so they show up on the Actions tab.
- Locally whenever you like: `node self-improvement/check.mjs`. It takes a few seconds.
- An AI in a Consilium conversation can be asked to "run the self-check and summarize new ledger entries". Only that summary step uses tokens.

## Ledger format

```json
{
  "id": "npm-audit-vite-GHSA-xxxx",
  "status": "open",
  "severity": "high",
  "source": "npm audit",
  "title": "vite: path traversal in dev server",
  "evidence": "vite <8.3.9, fix available",
  "found": "2026-11-14",
  "fix": null
}
```

`status` is `open`, `in-progress`, `fixed` or `wont-fix` (with a reason in `fix`). Evidence never includes secrets, personal paths or conversation content; `check.mjs` runs the privacy gate's redaction on it first.
