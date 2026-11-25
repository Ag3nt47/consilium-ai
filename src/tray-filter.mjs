// Which request-tray boxes to leave out while auto-run is on.

/**
 * Hide a box from the tray only when the backend is running it and this window didn't start it,
 * meaning auto-run started it. Boxes the human ran here (including install and admin boxes) stay.
 * @param {string} key `${eventId}#${index}`
 * @param {ReadonlySet<string>} backendRunning keys from runcmd::running_keys()
 * @param {ReadonlySet<string>} startedHere keys this window started with a Run click
 * @returns {boolean}
 */
export function hiddenFromTray(key, backendRunning, startedHere) {
  return backendRunning.has(key) && !startedHere.has(key);
}