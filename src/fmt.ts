/** Shared Intl formatters. `Number.toLocaleString` and `Date.toLocale*String` build a fresh
 *  formatter on every call; in a long chat that re-renders often, WebKit spent most of a core
 *  constructing them. These are built once and reused. */
const intFmt = new Intl.NumberFormat();
const hhmm = new Intl.DateTimeFormat([], { hour: "2-digit", minute: "2-digit" });
const hMm = new Intl.DateTimeFormat([], { hour: "numeric", minute: "2-digit" });
const weekday = new Intl.DateTimeFormat([], { weekday: "short" });
const dateTime = new Intl.DateTimeFormat(undefined, {
  year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric",
});

/** 12,345 */
export const num = (n: number): string => intFmt.format(n);
/** 03:40 PM */
export const clock = (ts: number | Date): string => hhmm.format(ts);
/** 3:40 PM */
export const clockShort = (ts: number | Date): string => hMm.format(ts);
/** Mon */
export const day = (ts: number | Date): string => weekday.format(ts);
/** 11/18/2026, 3:40:12 PM */
export const dateAndTime = (ts: number | Date): string => dateTime.format(ts);
