// Wording that differs between macOS, Windows and Linux, so every system reads naturally.
const ua = typeof navigator === "undefined" ? "" : `${navigator.platform} ${navigator.userAgent}`;

export const OS: "mac" | "windows" | "linux" = /Mac|iPhone|iPad/i.test(ua) ? "mac" : /Win/i.test(ua) ? "windows" : "linux";

/** The palette shortcut as this system writes it. */
export const paletteKey = OS === "mac" ? "⌘K" : "Ctrl+K";
export const paletteKeyWords = OS === "mac" ? "Cmd+K" : "Ctrl+K";

/** The window setup steps open in. */
export const terminalName = OS === "windows" ? "PowerShell" : "Terminal";

/** How to dictate when in-app speech recognition isn't available. */
export const dictationHint =
  OS === "mac"
    ? "Press the Fn key twice (macOS Dictation) in the message box to speak"
    : OS === "windows"
      ? "Press Windows+H (Windows voice typing) in the message box to speak"
      : "Use your desktop's dictation tool in the message box to speak";
