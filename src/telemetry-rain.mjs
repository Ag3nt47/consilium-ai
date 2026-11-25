/** Animation timing and completion freshness; neither helper reads host state. */
export const COMPLETION_TTL_MS = 8_000;

export function isRecentCompletion(timestamp, now) {
  return Number.isFinite(timestamp) && Number.isFinite(now)
    && timestamp >= 0 && now >= timestamp && now - timestamp < COMPLETION_TTL_MS;
}

/** Keep the rain dense while bounding the number of painted pixels. */
export function rainLayout(width, height, deviceRatio, eco) {
  const count = Math.min(eco ? 64 : 96, Math.max(1, Math.floor(width / (eco ? 32 : 22))));
  const budget = eco ? 1_000_000 : 3_000_000;
  const ratio = Math.min(deviceRatio || 1, eco ? 1 : 2, Math.sqrt(budget / Math.max(1, width * height)));
  return { count, ratio };
}

export class RainClock {
  #lastSeen = null;
  #lastPaint = null;
  #nextPaint = null;
  #fps = null;

  reset() {
    this.#lastSeen = this.#lastPaint = this.#nextPaint = this.#fps = null;
  }

  /** Return seconds since the prior paint, or null when no paint is due. */
  advance(now, fps) {
    const interval = 1000 / fps;
    if (!Number.isFinite(now) || now < 0 || !Number.isFinite(fps)
      || fps <= 0 || !Number.isFinite(interval) || interval <= 0) {
      this.reset();
      return null;
    }
    if (this.#lastSeen !== null && now < this.#lastSeen) this.reset();
    this.#lastSeen = now;
    if (this.#lastPaint === null) {
      this.#lastPaint = now;
      this.#nextPaint = now + interval;
      this.#fps = fps;
      return 0;
    }
    if (fps !== this.#fps) {
      this.#fps = fps;
      this.#nextPaint = this.#lastPaint + interval;
    }
    // A small tolerance avoids dropping a frame solely through fractional
    // millisecond rounding at 60/120/144 Hz. Duplicate callbacks do no work.
    if (now <= this.#lastPaint || now + .001 < this.#nextPaint) return null;
    const elapsed = Math.min((now - this.#lastPaint) / 1000, .08);
    this.#lastPaint = now;
    // Advance the schedule, preserving its fractional remainder. A delayed
    // callback paints once, rather than replaying missed frames after a stall.
    const skipped = Math.max(1, Math.floor((now + .001 - this.#nextPaint) / interval) + 1);
    this.#nextPaint += skipped * interval;
    return elapsed;
  }
}
