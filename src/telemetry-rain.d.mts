export const COMPLETION_TTL_MS: 8000;
export function isRecentCompletion(timestamp: number, now: number): boolean;
export class RainClock {
  reset(): void;
  /** Seconds since the prior paint, capped at .08; null means skip this frame. */
  advance(now: number, fps: number): number | null;
}
