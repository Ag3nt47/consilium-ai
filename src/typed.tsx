import { useEffect, useRef, useState } from "react";

/** Themes that show the AIs' live activity as if a terminal were typing it out. */
export const TYPED_THEMES = new Set(["retro84", "netrunner", "crt", "cyber"]);

function reduceMotion(): boolean {
  return Boolean(window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) || document.documentElement.dataset.performance === "eco";
}

/**
 * Text that types itself out when it changes. Only the part after what's already on screen is typed,
 * so "Claude: Reading · a.ts" → "Claude: Reading · b.ts" retypes just the file name. Each change finishes
 * within about a third of a second, so fast-changing activity never falls behind. With `enabled` off,
 * reduced motion or eco mode, the text simply changes.
 */
export function TypedText({ text, enabled }: { text: string; enabled: boolean }) {
  const [shown, setShown] = useState(enabled ? "" : text);
  const shownRef = useRef(enabled ? "" : text);
  useEffect(() => {
    if (!enabled || reduceMotion()) {
      shownRef.current = text;
      setShown(text);
      return;
    }
    const from = shownRef.current;
    let common = 0;
    while (common < from.length && common < text.length && from[common] === text[common]) common++;
    const remaining = text.length - common;
    if (remaining <= 0) {
      shownRef.current = text;
      setShown(text);
      return;
    }
    // ~20 frames at most: a long line types in big steps, a short one character by character.
    const step = Math.max(1, Math.ceil(remaining / 20));
    let at = common;
    shownRef.current = text.slice(0, at);
    setShown(shownRef.current);
    const timer = window.setInterval(() => {
      at = Math.min(text.length, at + step);
      shownRef.current = text.slice(0, at);
      setShown(shownRef.current);
      if (at >= text.length) window.clearInterval(timer);
    }, 16);
    return () => {
      window.clearInterval(timer);
    };
  }, [text, enabled]);
  return <span className={enabled ? "typed" : undefined}>{enabled ? shown : text}</span>;
}
