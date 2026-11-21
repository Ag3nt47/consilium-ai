# Changelog

## Unreleased

## 0.3.55

- Estimate local model memory requirements and ask paired computers for current memory reports.
- Run the same memory calculator offline from a source checkout with `node scripts/model-fit.mjs`.
- Show GitHub update checks, download progress, and installer staging failures in Settings.
- Request draft Windows, macOS, and Linux installers from Settings for a main commit that passed CI.
- Use NSIS update mode on Windows and preserve staged installers when the expected version is missing after setup.
- Use shorter task-board instructions to reduce repeated prompt text.

## 0.3.54

- Guide AI setup and device pairing from the Add menu.
- Bring Consilium forward and focus the current draft with a global shortcut.
- Attach clipboard screenshots and refresh the daily wallpaper after wake or a date change.
- Set an optional daily Discuss token warning and skip background usage scans when the usage panel is closed.
- View progress across the six upgrade documents in Settings. Council handoffs use a shorter format.
- Show when Signal is still waiting for verification and stop a stale Signal daemon before relinking.

## 0.3.53

- Follow new chat messages while the reader is at the bottom, and keep their place when they scroll up.
- Reduce prompt size with exact council-memory deduplication and previews of older successful command results.
- Read replies from Signal subscriptions, including the reply used to verify setup.
- Keep switched-off council members out of resumed turns, stand-in selection, and linked-computer requests.
- Use source event IDs to prevent duplicate imports and preserve separate messages with identical text.
- Follow published macOS installers and keep the Signal settings address field on one line.

- Fixed: when a linked PC announced an update, macOS no longer starts a full source rebuild while
  the GitHub release is still publishing. It checks every 30 minutes (and immediately after the
  announcement), verifies the published DMG's SHA-256, stages its app bundle atomically, and lets
  the existing automatic installer restart into it (#10).

- Fixed: auto-continue went quiet with no note whenever any AI had a request open, even one it
  had withdrawn or one from a linked computer's AI (answered there). Those no longer hold rounds
  back, and when a request here really is waiting, it says so once and lists it (#21).

- Fixed: a freeze of up to a minute (and a freeze-guard restart) when messages arrived while
  AIs were answering. The turn table stayed locked while the window, linked computers, notes
  and the council were told of a message; it is now held only to file the message (#14).

- Fixed: the council list in the sidebar now fills the space down to PC pressure. With five or
  more AIs it was capped at 46% of the window (at most 480 px), leaving a gap above PC pressure
  and cutting cards off; it now takes the remaining height and scrolls inside it. Council card
  controls (start/stop, model, access) also sit inside each card, so cards use the full width.

- Fixed: conversations disappearing, or splitting into a new one with the same title. Saving a
  conversation's title file used one shared temporary name and wasn't flushed to disk, so the app
  and the terminal saving at once, a rename refused by an encrypted AppData folder, or a restart
  for an update could leave the file missing or zeroed, and the conversation vanished from the
  list. Saves now use a unique, flushed temporary file and never remove the old file first, and a
  missing or damaged title file is rebuilt from the conversation's own messages. Settings, Auto-run
  chats, council memory and custom AIs are saved the same way.
- **Zero Breach auto-remove skips running browsers**: extension cleanup is deferred while target
  browsers (Chrome, Edge, Brave, Vivaldi, Arc, Chromium) are open across macOS, Windows, and Linux
  to avoid profile corruption and orphaned extension records.
- **Extension remediation guidance**: scan summaries and findings advise confirming removal at
  `chrome://extensions` and resetting any hijacked search engine.

- **Auto-continue keeps going in auto-run chats**: in a chat with "Auto-run this chat" on,
  auto-continue no longer pauses after two rounds that changed nothing on the task board, since
  commands there report back by themselves. It still stops after 8 rounds, on an unanswered
  REQUEST in other chats, when no task is left, and on Stop.
- **Auto-run chats approve every request**: in a chat with "Auto-run this chat" on, each REQUEST
  is approved on your behalf about 20 seconds after it is asked (click Deny first to stop it), as
  many times as the AIs ask, so nothing waits for you. The request card shows it as approved.
- **Auto-run this chat**: a red button on command boxes turns on, after a warning, running every
  command box in that chat without asking, including deletes, overwrites, git history rewrites,
  installs and admin commands (the computer still shows its own password prompt). Only that chat is
  affected and every new chat starts with it off; a red "Auto-run on" badge turns it off. Commands
  written by another linked computer's AI still never run here.
- **Start/Stop each AI per chat**: a small green â–¶ / red â–  button next to each AI in the council
  boxes switches it on or off in this chat only. A stopped AI isn't asked by the box, an @mention,
  another AI's @mention or an auto-continue round until you press Start; Consilium notes who was
  skipped.
- **Messages go only to who they name**: a message that @mentions council members is sent only to
  them, so other AIs no longer start up for it.
- **Claude reset history**: claude-resets.com's history of Claude usage resets (CC BY 4.0) shows
  next to the Codex reset forecast. History only, no guessed chance.
- **Jump to latest**: a floating â†“ button appears when the chat is scrolled up (with a count of new
  messages), and long command boxes, now capped at 60% of the window, get a â†“ to jump to their end.
- **Tools stay found after updates**: Consilium reads the current search path (Windows' saved
  PATH, or the Mac/Linux login shell's) plus the usual tool folders, instead of the one it was
  started with, so installed AIs and tools no longer look uninstalled after an update. Codex in
  `AppData\Local\Programs\OpenAI\Codex` is found even off PATH.
- **Linked computers update together**: a computer installing an update tells the others, which
  prepare or download the same build at once and install it.
- **System-aware command boxes**: PowerShell and Command Prompt boxes get Run only on Windows, bash
  and zsh only on Mac and Linux; others are marked "for Windows" or "for Mac/Linux" with Copy. A
  command written by another linked computer's AI is marked for that computer and never runs here.
- **Updates install and restart by themselves**: a new setting (Settings → Updates, on by default,
  including for existing installs) installs a newer build as soon as it is ready and restarts;
  the AIs' work resumes after. It waits while a command you started is running. Consilium checks
  GitHub every 30 minutes, and linked computers move to the newest build any of them runs.
- **Old requests clear themselves**: an AI can withdraw a request it no longer needs with
  `WITHDRAW: <request>` or `WITHDRAW: all`, and is reminded each turn which of its requests are
  still waiting. Requests and command boxes older than a day leave the request tray; they stay
  in the chat.
- **Communication stress test**: a button with the linked computers (and after every restart)
  moves the link onto each route in turn, shutting the others out, checks that traffic comes back
  over it, returns to the fastest route, and posts the results in the chat.
- **AIs address each other across computers**: every AI on another linked computer has a handle
  with its computer's name, such as `@claude-lukes-mbp`, and every AI is told the handles. A
  handle reaches only that AI, which joins in and answers (up to three such turns per message you
  send). A plain `@claude` still means the AI on your own computer.
- **AI handshake**: the linked-computers panel lists every AI on both computers with a green ✓
  once it has read the other computer's AIs and been read by them.
- **Always run holds back risky commands**: a box that deletes or overwrites files, rewrites git
  history, uninstalls software, stops programs or the computer, changes folder permissions or runs
  a downloaded script waits for your click even with Always run on, with a note saying why. Before,
  such a box could run before a Deny arrived.
- Fixed: a message typed from the terminal into another computer's conversation stayed on this
  computer. The open app now sends it on.
- **Linked computers stay on the same build**: every build now has a build number and commit
  (e.g. "0.3.37 #812 (76debf3a1b2c)"), so two builds of one version are told apart. An update is
  only offered over an older build, and staging never replaces a newer staged build, so updates
  can't be overridden. Linked computers show each other's build, and one that is behind fetches
  the newest one's exact commit from GitHub into Consilium's own folder, builds and stages it, and
  says so in the conversation; it then installs like any update and the AIs carry on.
- **Linked computers work together**: the AIs on every linked computer now answer in the same
  conversation. Your message reaches the other computer's AIs, their answers come back into your
  conversation as "Claude (MacBook)" and so on, and each side's AIs read the other's answers. When
  a side's AIs have finished, they get one more turn to react, at most once per message you send,
  so two councils can never keep each other going. Conversations started before the computers were
  linked, or before a new one was paired, are copied over in full, so its AIs can join in.
- **Attachments**: attach documents, Word files, pictures, PDFs, code or anything else to a message:
  📎 in the message box, drag and drop onto the window, or paste a screenshot; `--attach FILE` and
  `/attach` in the terminal. Consilium keeps its own copy with the conversation. Text files, and the
  text read out of Word (.docx), OpenDocument (.odt) and PowerPoint (.pptx) files, go to every AI;
  ChatGPT gets pictures directly, and Claude opens pictures and PDFs. Gemini (Antigravity) has no
  supported way to take a picture in, so it is told so and leaves pictures to the others.
- Fixed: the "data: codex-reset.com" link couldn't open (the app only allowed GitHub links).
- **Terminal version**: `consilium chat`, `ask`, `say`, `log`, `list`, `new`, `stop`, `status` and `check`,
  in the same conversations, council, memory and settings as the app. Use either, or both at once:
  a conversation's AIs run in one process at a time, a message sent from the other joins that turn,
  and Stop works from either. The app shows what the terminal adds as it happens. Install it from
  Settings → Terminal or with `consilium install-cli`; the install scripts set it up.
- **Usage per model and overall**: each AI's card shows the model in use (it follows a switch, for
  example from Fable to Opus) with the AI's whole plan underneath, and when each limit resets.
- **The AIs know everyone's usage**: each AI is told every member's model and overall usage left and
  reset times, so they can plan around an AI that's out until its reset.
- Optional, off by default: codex-reset.com's unofficial forecast of the next global Codex reset,
  shown on ChatGPT's card and given to the AIs.
- Show each AI's provider-reported plan allowance remaining from 0–100%, using the tightest active
  quota window and exposing all reported windows in the meter details.
- Refresh provider status concurrently and coalesce overlapping UI refreshes, reducing startup and
  post-turn wait time without spending model quota.
- Record provider-reported token counts on each AI reply and expose per-conversation and per-day
  totals for the upcoming Usage panel.
- Keep conversation navigation, settings and AI status visible at the native window's 760px minimum.
- A proposal to add an AI (`ADD_AI:`) always waits for the human's approval; the option to let AIs add others without asking is gone.
- Update the direct `dirs` dependency to version 7 and refresh compatible Rust dependencies.

## 0.2.1

- **Sending doesn't interrupt**: a message sent while the AIs are answering joins the running turn. Every AI reads it (one in the middle of an answer, as soon as it has finished) and gets a fresh set of answers, and anyone it addresses who wasn't taking part joins in. **Stop** is what interrupts.

## 0.2.0

- **No turn order**: every AI answers as soon as it's ready and reacts to new messages on its own time, instead of waiting for the others. In a discussion each AI answers up to the round limit, and the discussion ends when nobody has anything new to say.
- **Out of usage doesn't stop the conversation**: an AI whose CLI reports a usage limit, rate limit or quota sits out until the limit resets (read from the CLI's message, otherwise 30 minutes), and the others carry on. If nobody else is answering, another council member takes its turn. The Council panel shows "Out of usage · back 3:40 PM" with **Retry**.
- **Stop and interrupt always work**: they finish within a few seconds, even if an AI left a program running (such as a dev server) that kept its output open, which used to leave the conversation stuck as "still answering". Sending while the AIs are answering always interrupts them, and a message is never refused.
- **Resume after restart is more robust**: a turn's progress is read back from the conversation, so nobody repeats an answer, and turns resumed after updates aren't given up on.
- Notes from Consilium (an AI out of usage, a restart, an update) are shown to the AIs as [Consilium] lines; after an update, a note says what changed.
- Saves of conversations, settings, memory and the resume record fall back to copying when Windows refuses to replace the file, so a turn can't be resumed twice.

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
- **+ Add AI**: add Gemini (Google's Gemini CLI, your Google sign-in), remove members, with install and sign-in steps in one place. AIs can propose a member with an `ADD_AI:` line; the human approves every addition.
- Project folders and per-AI access (Chat only, Read, Edit, Full); a **Full capacity** preset; build-mode teamwork instructions for the AIs.
- Gemini runs through Google's Antigravity CLI (Google ended Gemini CLI access for personal accounts in June 2026); the Gemini CLI remains the fallback for company plans and API keys.
- **self-check** in the Council panel: one tiny test message to every AI at once, with the result and time on each card.
- Unrestricted access level (each tool's own bypass switch); a council-wide default access, including for AIs added later; a default project folder; Full capacity now sets Unrestricted.
- Council memory (REMEMBER: lines, editable in Settings) and per-project CONSILIUM.md notes, shared by every AI.
- An AI that @mentions another council member brings it into the same turn (at most two per round); everyone in the council is asked by default after a restart.
- Restart resilience: on macOS closing the window keeps the AIs working; a turn cut short by a quit or crash resumes on the next launch.
