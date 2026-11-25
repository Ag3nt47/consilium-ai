# Consilium deployment vision

Consilium is intended to coordinate AI agents and tools across the computers you already own. A Windows workstation, a MacBook Pro, and additional paired nodes could share a task board and route work based on available memory, tools, access, and verified connectivity.

The [upgrade ledger](UPGRADE_STATUS.md) tracks implementation and verification. The current wired split-model experiments establish feasibility on two computers. The workflows below describe future deployments.

| Workflow | Deployment concept | What would demonstrate readiness |
| --- | --- | --- |
| Software engineering | Split a feature into backend, interface, and test tasks. Give each worker a worktree and clear file scope. Run OS-specific builds on the appropriate host and review results together. | Concurrent edits remain isolated, results arrive with their commit and test evidence, and an interrupted build resumes or retries without repeating accepted work. |
| Repository maintenance | Queue inexpensive local checks overnight. Bring regressions, dependency advisories, and tested patch proposals to a review inbox. | Resource limits hold while idle and active, duplicate work is suppressed, and each proposal identifies the evidence and change under review. |
| Offline operation and quota fallback | Pair machines on available local links. If a cloud provider runs out of quota, pass a compact checkpoint to a compatible local runner. Resume cloud work when allowed and useful. | An offline-only session completes without a network dependency. A runner or route interruption preserves accepted results and makes the next step visible. |
| Defensive monitoring | Parse local telemetry, summarize suspicious events, and review draft detection rules against synthetic or previously recorded defensive datasets. Run tabletop exercises with simulated alerts and reviewed containment decisions. | A replay fixture produces reproducible alerts, private logs stay within the intended trust boundary, and proposed responses have clear evidence and approval history. |
| Local research and publishing | Index selected notes, PDFs, and code locally. Use source-linked answers for research. Separate canon review, editorial review, and document export in a writing workflow. | Retrieval returns inspectable sources, respects selected folders, and can produce a verified export against an explicit publishing checklist. |
| Research backtests and daily briefing | Distribute reproducible historical simulations and assemble a morning report with system health, repository activity, and selected research results. | Results identify their inputs, assumptions, and test conditions. Each briefing entry links to source evidence and distinguishes simulation output from observed events. |

Device discovery and transport failover, hardware-aware model placement, resumable checkpoints, and a clear review history are shared dependencies across these workflows. Verify each dependency with bounded tests before describing it as supported.
