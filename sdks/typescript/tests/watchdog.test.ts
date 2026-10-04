/**
 * Tests for the core-liveness watchdog.
 *
 * An orphaned host keeps serving its platform after the Core dies, and once a new Core starts
 * both processes handle the same messages. These tests pin the two outcomes that matter: a
 * reachable Core never triggers a stop, and a Core that stops answering triggers exactly one.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { startCoreWatchdog } from "../src/sdk/watchdog.js";

/** Resolves after `ms`, keeping the event loop alive for the watchdog's timers. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

test("stops the host after consecutive probe failures", async () => {
  const reasons: string[] = [];
  const stop = startCoreWatchdog(
    {
      ping: async () => {
        throw new Error("core is gone");
      },
    },
    {
      intervalMs: 5,
      failuresBeforeStop: 2,
      onLost: (reason) => reasons.push(reason),
      log: () => {},
    },
  );

  await sleep(100);
  stop();

  assert.equal(reasons.length, 1, "the host must be told exactly once");
  assert.match(reasons[0], /core unreachable/);
});

test("keeps probing while the core answers", async () => {
  let pings = 0;
  const reasons: string[] = [];
  const stop = startCoreWatchdog(
    {
      ping: async () => {
        pings += 1;
      },
    },
    {
      intervalMs: 5,
      failuresBeforeStop: 2,
      onLost: (reason) => reasons.push(reason),
      log: () => {},
    },
  );

  await sleep(100);
  stop();

  assert.ok(pings > 1, `expected repeated probes, saw ${pings}`);
  assert.deepEqual(reasons, []);
});

test("recovers from a transient failure without stopping", async () => {
  let attempt = 0;
  const reasons: string[] = [];
  const stop = startCoreWatchdog(
    {
      ping: async () => {
        attempt += 1;
        // Exactly one failure, then the core answers again: a restart window must not kill the
        // host, because the new core will spawn its own hosts anyway and a healthy core is busy.
        if (attempt === 1) throw new Error("temporarily unavailable");
      },
    },
    {
      intervalMs: 5,
      failuresBeforeStop: 3,
      onLost: (reason) => reasons.push(reason),
      log: () => {},
    },
  );

  await sleep(120);
  stop();

  assert.ok(attempt >= 2, `expected further probes, saw ${attempt}`);
  assert.deepEqual(reasons, []);
});

test("serializes in-flight probes and ignores their failure after stop", async () => {
  let pings = 0;
  let failProbe: ((error: Error) => void) | undefined;
  const reasons: string[] = [];
  const stop = startCoreWatchdog(
    {
      ping: async () => {
        pings += 1;
        await new Promise<void>((_resolve, reject) => { failProbe = reject; });
      },
    },
    { intervalMs: 5, failuresBeforeStop: 1, onLost: (reason) => reasons.push(reason), log: () => {} },
  );

  await sleep(40);
  stop();
  assert.equal(pings, 1, "an unfinished probe must not start another RPC");
  failProbe!(new Error("channel closed during shutdown"));
  await sleep(60);

  assert.equal(pings, 1, "no probe may run after stop()");
  assert.deepEqual(reasons, [], "a stopped watchdog must not trigger another shutdown");
});
