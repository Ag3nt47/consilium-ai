# Changelog

## 0.2.0

- **No turn order**: every AI answers as soon as it's ready and reacts to new messages on its own time, instead of waiting for the others. In a discussion each AI answers up to the round limit, and the discussion ends when nobody has anything new to say.
- **Out of usage doesn't stop the conversation**: an AI whose CLI reports a usage limit, rate limit or quota sits out until the limit resets (read from the CLI's message, otherwise 30 minutes), and the others carry on. If nobody else is answering, another council member takes its turn. The Council panel shows "Out of usage · back 3:40 PM" with **Retry**.
- **Stop and interrupt always work**: they finish within a few seconds, even if an AI left a program running (such as a dev server) that kept its output open, which used to leave the conversation stuck as "still answering". Sending while the AIs are answering always interrupts them, and a message is never refused.
- **Resume after restart is more robust**: a turn's progress is read back from the conversation, so nobody repeats an answer, and turns resumed after updates aren't given up on.
- Notes from Consilium (an AI out of usage, a restart, an update) are shown to the AIs as [Consilium] lines; after an update, a note says what changed.

## 0.1.0

First release.

- ChatGPT (through the Codex CLI) and Claude (through Claude Code) in one shared, color-coded conversation, using your existing sign-ins.
- Ask and Discuss modes with a round cap, early stop, and Stop.
- Red `REQUEST:` callouts for questions an AI needs you to answer.
- Themes: Consilium, Cyber, Slate, CRT, Retro '84.
- Set up AIs: one-click install or sign-in for a missing AI.
- Optional daily check for new releases (notify only).
- Copy a conversation as Markdown; rename and delete conversations.
- Interrupt: send while an AI is answering to stop it and send your new message; switching Discuss off ends a running discussion.
- The AIs' status is re-checked when the window regains focus, so a finished sign-in shows up without a restart.
- Demo mode for screenshots and UI work in a plain browser, with invented data.
- **+ Add AI**: add Gemini (Google's Gemini CLI, your Google sign-in), remove members, with install and sign-in steps in one place. AIs can propose a member with an `ADD_AI:` line; optionally let them add without asking.
- Project folders and per-AI access (Chat only, Read, Edit, Full); a **Full capacity** preset; build-mode teamwork instructions for the AIs.
- Gemini runs through Google's Antigravity CLI (Google ended Gemini CLI access for personal accounts in June 2026); the Gemini CLI remains the fallback for company plans and API keys.
- **self-check** in the Council panel: one tiny test message to every AI at once, with the result and time on each card.
- Unrestricted access level (each tool's own bypass switch); a council-wide default access, including for AIs added later; a default project folder; Full capacity now sets Unrestricted.
- Council memory (REMEMBER: lines, editable in Settings) and per-project CONSILIUM.md notes, shared by every AI.
- An AI that @mentions another council member brings it into the same turn (at most two per round); everyone in the council is asked by default after a restart.
- Restart resilience: on macOS closing the window keeps the AIs working; a turn cut short by a quit or crash resumes on the next launch.
