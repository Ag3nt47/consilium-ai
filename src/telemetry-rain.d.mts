export const COMPLETION_TTL_MS: 8000;
export function isRecentCompletion(timestamp: number, now: number): boolean;
export class RainClock {
  reset(): void;
  /** Seconds since the prior paint, capped at .08; null means skip this frame. */
  advance(now: number, fps: number): number | null;
}
export declare function rainLayout(width: number, height: number, deviceRatio: number, eco: boolean): { count: number; ratio: number };
