/**
 * Core-liveness watchdog for the TypeScript plugin host.
 *
 * A plugin host is a child of the Core, but parentage is not a liveness guarantee: a Core
 * killed with `SIGKILL`, or whose parent terminal disappears, leaves its hosts running. Such
 * an orphan keeps serving its platform (a gateway socket, a long-poller) while nothing can
 * answer — and once a new Core starts, the orphan and the fresh host both serve the same
 * platform, so every inbound message is handled twice.
 *
 * The watchdog probes `BotApiService.Ping`. Any answer proves the Core is alive; after
 * `failuresBeforeStop` consecutive failures the host is stopped, which unloads the plugin and
 * closes its platform connections.
 */

/** Everything the watchdog needs from a Core handle. */
export interface LivenessProbeTarget {
  ping(): Promise<void>;
}

/** Watchdog tuning. */
export interface CoreWatchdogOptions {
  /** Delay between probes in milliseconds. */
  intervalMs?: number;
  /** Consecutive failed probes tolerated before the Core is declared lost. */
  failuresBeforeStop?: number;
  /** Invoked exactly once when the Core is considered lost. */
  onLost: (reason: string) => void;
  /** Sink for progress logs; defaults to `console.warn`. */
  log?: (message: string) => void;
}

/** Default cadence: three failures at 15s mean roughly 45s of silence. */
export const DEFAULT_WATCHDOG_INTERVAL_MS = 15_000;
export const DEFAULT_WATCHDOG_FAILURES = 3;

/**
 * Starts probing the Core and returns a stop function.
 *
 * The returned function clears the timer; call it during a graceful shutdown so a host that
 * stops for its own reasons does not leave a pending probe behind.
 */
export function startCoreWatchdog(
  target: LivenessProbeTarget,
  options: CoreWatchdogOptions,
): () => void {
  const intervalMs = options.intervalMs ?? DEFAULT_WATCHDOG_INTERVAL_MS;
  const failuresBeforeStop = options.failuresBeforeStop ?? DEFAULT_WATCHDOG_FAILURES;
  const log = options.log ?? ((message: string) => console.warn(message));

  let failures = 0;
  let stopped = false;
  let probing = false;

  const timer = setInterval(() => {
    if (stopped || probing) return;
    probing = true;

    void target
      .ping()
      .then(() => {
        failures = 0;
      })
      .catch((err: unknown) => {
        if (stopped) return;
        failures += 1;
        const message = err instanceof Error ? err.message : String(err);
        log(
          `Core liveness probe failed (${failures}/${failuresBeforeStop}): ${message}`,
        );

        if (failures >= failuresBeforeStop && !stopped) {
          stopped = true;
          clearInterval(timer);
          options.onLost(
            `core unreachable for ${failures} consecutive probes; stopping this host so it cannot serve its platform without a core`,
          );
        }
      })
      .finally(() => { probing = false; });
  }, intervalMs);

  // Probing must never be the reason the process stays alive.
  timer.unref?.();

  return () => {
    stopped = true;
    clearInterval(timer);
  };
}
