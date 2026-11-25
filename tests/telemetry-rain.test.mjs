import assert from "node:assert/strict";
import test from "node:test";
import { COMPLETION_TTL_MS, isRecentCompletion, RainClock, rainLayout } from "../src/telemetry-rain.mjs";

test("rain fills narrow and standard screens even when running in Eco mode", () => {
  for (const [width, normal, eco] of [[420, 19, 13], [1200, 54, 37]]) {
    assert.equal(rainLayout(width, 720, 1, false).count, normal);
    assert.equal(rainLayout(width, 720, 1, true).count, eco);
  }
  assert.equal(rainLayout(20, 720, 1, true).count, 1);
});

test("wide high-DPI displays keep a bounded animated backing surface", () => {
  for (const [eco, maxColumns, maxPixels] of [[false, 96, 3_000_000], [true, 64, 1_000_000]]) {
    const { count, ratio } = rainLayout(3840, 2160, 2, eco);
    assert.equal(count, maxColumns);
    assert.ok(Math.floor(3840 * ratio) * Math.floor(2160 * ratio) <= maxPixels);
  }
});

for (const refresh of [60, 120, 144]) {
  for (const fps of [15, 30, 60]) {
    test(`${fps} FPS rain retains its cadence on a ${refresh} Hz display`, () => {
      const clock = new RainClock();
      const duration = 10;
      let paints = 0;
      let movement = 0;
      for (let frame = 0; frame <= refresh * duration; frame++) {
        const elapsed = clock.advance(frame * 1000 / refresh, fps);
        if (elapsed !== null) {
          paints++;
          movement += elapsed;
          assert.ok(elapsed >= 0 && elapsed <= .08);
        }
      }
      assert.ok(Math.abs(paints - (fps * duration + 1)) <= 1, `Observed ${paints} paints`);
      assert.ok(Math.abs(movement - duration) <= 1 / fps + .001, `Advanced ${movement}s`);
    });
  }
}

test("missed display callbacks paint once with bounded movement instead of catching up", () => {
  const clock = new RainClock();
  assert.equal(clock.advance(0, 30), 0);
  assert.equal(clock.advance(10_000, 30), .08);
  assert.equal(clock.advance(10_000, 30), null);
  assert.equal(clock.advance(10_010, 30), null);
  const next = clock.advance(10_034, 30);
  assert.ok(next > .03 && next < .04);
});

test("resetting a hidden or paused animation discards old movement", () => {
  const clock = new RainClock();
  clock.advance(0, 30);
  clock.advance(34, 30);
  clock.reset();
  assert.equal(clock.advance(60_000, 30), 0);
  assert.equal(clock.advance(60_010, 30), null);
  assert.ok(clock.advance(60_034, 30) > .03);
});

test("changing FPS respects the new cadence and retains real elapsed movement", () => {
  const clock = new RainClock();
  assert.equal(clock.advance(0, 60), 0);
  assert.equal(clock.advance(10, 60), null);
  assert.equal(clock.advance(20, 60), .02);
  assert.equal(clock.advance(30, 15), null);
  assert.equal(clock.advance(80, 15), null);
  assert.equal(clock.advance(90, 15), .07);
  assert.equal(clock.advance(100, 60), null);
  assert.equal(clock.advance(110, 60), .02);
});

test("invalid timestamps or frame rates cannot advance the animation", () => {
  for (const value of [NaN, Infinity, -Infinity, -1, "30", null, undefined]) {
    const clock = new RainClock();
    clock.advance(0, 30);
    assert.equal(clock.advance(value, 30), null);
    assert.equal(clock.advance(100, 30), 0);
    assert.equal(clock.advance(120, value), null);
    assert.equal(clock.advance(140, 30), 0);
  }
  const clock = new RainClock();
  assert.equal(clock.advance(100, 0), null);
});

test("a backward monotonic timestamp starts a new baseline without reverse movement", () => {
  const clock = new RainClock();
  assert.equal(clock.advance(100, 30), 0);
  assert.ok(clock.advance(134, 30) > 0);
  assert.equal(clock.advance(50, 30), 0);
  assert.equal(clock.advance(50, 30), null);
  assert.ok(clock.advance(84, 30) > 0);
});

test("completion evidence expires at eight seconds and excludes future timestamps", () => {
  const now = 100_000;
  assert.equal(COMPLETION_TTL_MS, 8000);
  assert.equal(isRecentCompletion(now, now), true);
  assert.equal(isRecentCompletion(now - 7_999, now), true);
  assert.equal(isRecentCompletion(now - 8_000, now), false);
  assert.equal(isRecentCompletion(now - 8_001, now), false);
  assert.equal(isRecentCompletion(now + 1, now), false);
});

test("invalid completion times never become current telemetry", () => {
  for (const value of [NaN, Infinity, -Infinity, -1, "100", null, undefined]) {
    assert.equal(isRecentCompletion(value, 100), false);
    assert.equal(isRecentCompletion(0, value), false);
  }
});
