/**
 * coreHealthMonitor — polls the core server's public HTTP liveness endpoint
 * and dispatches `setCore` to the connectivitySlice (#1527).
 *
 * Polling cadence is adaptive:
 *   - healthy   : 30s (cheap heartbeat)
 *   - degraded  : 5s (fast recovery detection)
 *
 * A single transient failure is not enough to flip the channel — we require
 * `FAIL_THRESHOLD` consecutive failures to mark `unreachable` so a single
 * dropped TCP packet doesn't pop a scary blocking screen.
 */
import { setCore } from '../store/connectivitySlice';
import { store } from '../store/index';
import { getCoreHttpBaseUrl } from './coreRpcClient';

const HEALTHY_INTERVAL_MS = 30_000;
const DEGRADED_INTERVAL_MS = 5_000;
const FAIL_THRESHOLD = 2;
const PROBE_TIMEOUT_MS = 5_000;

let timer: ReturnType<typeof setTimeout> | null = null;
let consecutiveFails = 0;
let stopped = true;

class CoreHealthProbeError extends Error {
  readonly status?: number;

  constructor(message: string, status?: number) {
    super(message);
    this.name = 'CoreHealthProbeError';
    this.status = status;
  }
}

function healthUrl(baseUrl: string, path: '/health/live' | '/health'): string {
  const normalizedBase = baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`;
  return new URL(path.replace(/^\//, ''), normalizedBase).toString();
}

async function fetchHealthPath(baseUrl: string, path: '/health/live' | '/health'): Promise<void> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), PROBE_TIMEOUT_MS);
  try {
    const response = await fetch(healthUrl(baseUrl, path), {
      method: 'GET',
      cache: 'no-store',
      signal: controller.signal,
    });

    if (!response.ok) {
      throw new CoreHealthProbeError(`Core health HTTP ${response.status}`, response.status);
    }

    const payload = (await response.json().catch(() => null)) as { ok?: unknown } | null;
    if (!payload || payload.ok !== true) {
      throw new CoreHealthProbeError('Core health response did not report ok=true');
    }
  } catch (err) {
    if (controller.signal.aborted) {
      throw new CoreHealthProbeError(`Core health probe timed out after ${PROBE_TIMEOUT_MS}ms`);
    }
    throw err;
  } finally {
    clearTimeout(timeoutId);
  }
}

async function probeCoreHealth(): Promise<void> {
  const baseUrl = await getCoreHttpBaseUrl();
  try {
    await fetchHealthPath(baseUrl, '/health/live');
  } catch (err) {
    // Older cores only expose `/health`, and older auth allow-lists may
    // protect `/health/live`. Fall back to the legacy endpoint only for
    // endpoint-shape failures; transport failures still mean the core is down.
    if (err instanceof CoreHealthProbeError && (err.status === 401 || err.status === 404)) {
      await fetchHealthPath(baseUrl, '/health');
      return;
    }
    throw err;
  }
}

async function probe(): Promise<void> {
  try {
    await probeCoreHealth();
    consecutiveFails = 0;
    store.dispatch(setCore({ value: 'reachable' }));
  } catch (err) {
    consecutiveFails += 1;
    const message = err instanceof Error ? err.message : String(err);
    if (consecutiveFails >= FAIL_THRESHOLD) {
      store.dispatch(setCore({ value: 'unreachable', error: message }));
    }
  } finally {
    if (!stopped) schedule();
  }
}

function schedule(): void {
  if (timer != null) clearTimeout(timer);
  // Use the failure streak (not just the Redux state) so we enter degraded
  // 5s polling on the *first* miss — before the threshold flips `core` to
  // `unreachable`. Without this, first-failure retries stayed at 30s.
  // (addresses @coderabbitai on coreHealthMonitor.ts:46)
  const state = store.getState().connectivity.core;
  const isDegraded = consecutiveFails > 0 || state !== 'reachable';
  const interval = isDegraded ? DEGRADED_INTERVAL_MS : HEALTHY_INTERVAL_MS;
  timer = setTimeout(() => void probe(), interval);
}

export function startCoreHealthMonitor(): void {
  if (!stopped) return;
  stopped = false;
  consecutiveFails = 0;
  void probe();
}

export function stopCoreHealthMonitor(): void {
  stopped = true;
  if (timer != null) {
    clearTimeout(timer);
    timer = null;
  }
}
