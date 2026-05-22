/**
 * Tests for coreHealthMonitor.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Mock store and connectivitySlice first.
const dispatchMock = vi.fn();
vi.mock('../../store/index', () => ({
  store: { dispatch: dispatchMock, getState: () => ({ connectivity: { core: 'reachable' } }) },
}));

const setCoreMock = vi.fn((payload: unknown) => ({ type: 'connectivity/setCore', payload }));
vi.mock('../../store/connectivitySlice', () => ({ setCore: (p: unknown) => setCoreMock(p) }));

const getCoreHttpBaseUrlMock = vi.fn();
vi.mock('../coreRpcClient', () => ({ getCoreHttpBaseUrl: getCoreHttpBaseUrlMock }));

const fetchMock = vi.fn();

function okHealthResponse(): Response {
  return { ok: true, status: 200, json: async () => ({ ok: true, status: 'live' }) } as Response;
}

function healthResponse(status: number): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => ({ ok: false }),
  } as Response;
}

/** Flush all pending microtasks (resolved promises). */
async function flushPromises(): Promise<void> {
  // Multiple rounds handle chained .then() callbacks.
  for (let i = 0; i < 10; i++) {
    await Promise.resolve();
  }
}

describe('coreHealthMonitor', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.resetModules();
    dispatchMock.mockClear();
    setCoreMock.mockClear();
    getCoreHttpBaseUrlMock.mockReset();
    getCoreHttpBaseUrlMock.mockResolvedValue('http://127.0.0.1:7788');
    fetchMock.mockReset();
    vi.stubGlobal('fetch', fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it('startCoreHealthMonitor probes immediately on start', async () => {
    fetchMock.mockResolvedValueOnce(okHealthResponse());

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();

    // Flush micro-tasks so the async probe runs.
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledWith(
      'http://127.0.0.1:7788/health/live',
      expect.objectContaining({ method: 'GET', cache: 'no-store' })
    );
    stopCoreHealthMonitor();
  });

  it('dispatches reachable on successful probe', async () => {
    fetchMock.mockResolvedValueOnce(okHealthResponse());

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();
    await flushPromises();

    expect(setCoreMock).toHaveBeenCalledWith({ value: 'reachable' });
    stopCoreHealthMonitor();
  });

  it('does not dispatch unreachable until FAIL_THRESHOLD consecutive failures', async () => {
    // First failure — below threshold (2), should NOT dispatch unreachable yet.
    fetchMock.mockRejectedValueOnce(new Error('ECONNREFUSED'));

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();
    await flushPromises();

    // Only 1 failure, threshold is 2 — unreachable must NOT have been dispatched.
    const unreachableCalls = setCoreMock.mock.calls.filter(
      ([arg]) => (arg as { value: string }).value === 'unreachable'
    );
    expect(unreachableCalls).toHaveLength(0);
    stopCoreHealthMonitor();
  });

  it('dispatches unreachable after FAIL_THRESHOLD consecutive failures', async () => {
    // Two consecutive failures → should cross the threshold.
    fetchMock
      .mockRejectedValueOnce(new Error('ECONNREFUSED first'))
      .mockRejectedValueOnce(new Error('ECONNREFUSED second'));

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();

    // First probe.
    await flushPromises();

    // Advance timer to trigger the degraded-mode 5 s retry.
    vi.advanceTimersByTime(5_001);
    await flushPromises();

    const unreachableCalls = setCoreMock.mock.calls.filter(
      ([arg]) => (arg as { value: string }).value === 'unreachable'
    );
    expect(unreachableCalls.length).toBeGreaterThanOrEqual(1);
    stopCoreHealthMonitor();
  });

  it('is idempotent — second startCoreHealthMonitor call is a no-op', async () => {
    fetchMock.mockResolvedValue(okHealthResponse());

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();
    startCoreHealthMonitor(); // second call must not double-probe

    await flushPromises();

    // Only 1 probe should have fired.
    expect(fetchMock).toHaveBeenCalledTimes(1);
    stopCoreHealthMonitor();
  });

  it('stopCoreHealthMonitor prevents further scheduling', async () => {
    fetchMock.mockResolvedValue(okHealthResponse());

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();
    await flushPromises();

    const firstCallCount = fetchMock.mock.calls.length;
    stopCoreHealthMonitor();

    // Advancing time should not trigger another probe.
    vi.advanceTimersByTime(60_000);
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledTimes(firstCallCount);
  });

  it('schedule picks degraded interval when consecutiveFails > 0', async () => {
    // Make probe fail once so consecutiveFails becomes 1.
    fetchMock.mockRejectedValueOnce(new Error('connection refused')).mockResolvedValue({});

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();
    await flushPromises();

    // After 1 failure the next poll should be at DEGRADED_INTERVAL_MS = 5s, not 30s.
    vi.advanceTimersByTime(5_001);
    await flushPromises();

    // Second probe should have fired (recovery check).
    expect(fetchMock).toHaveBeenCalledTimes(2);
    stopCoreHealthMonitor();
  });

  it('error message is extracted from Error instance', async () => {
    fetchMock
      .mockRejectedValueOnce(new Error('timeout msg'))
      .mockRejectedValueOnce(new Error('timeout msg'));

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();
    await flushPromises();

    vi.advanceTimersByTime(5_001);
    await flushPromises();

    const unreachableCall = setCoreMock.mock.calls.find(
      ([arg]) => (arg as { value: string }).value === 'unreachable'
    );
    expect(unreachableCall).toBeDefined();
    expect((unreachableCall![0] as { error: string }).error).toBe('timeout msg');
    stopCoreHealthMonitor();
  });

  it('error message falls back to String(err) when not an Error instance', async () => {
    fetchMock
      .mockRejectedValueOnce('plain string error')
      .mockRejectedValueOnce('plain string error');

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();
    await flushPromises();

    vi.advanceTimersByTime(5_001);
    await flushPromises();

    const unreachableCall = setCoreMock.mock.calls.find(
      ([arg]) => (arg as { value: string }).value === 'unreachable'
    );
    expect(unreachableCall).toBeDefined();
    expect((unreachableCall![0] as { error: string }).error).toBe('plain string error');
    stopCoreHealthMonitor();
  });

  it('falls back to legacy /health when /health/live is unavailable', async () => {
    fetchMock.mockResolvedValueOnce(healthResponse(404)).mockResolvedValueOnce(okHealthResponse());

    const { startCoreHealthMonitor, stopCoreHealthMonitor } = await import('../coreHealthMonitor');
    startCoreHealthMonitor();
    await flushPromises();

    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      'http://127.0.0.1:7788/health/live',
      expect.objectContaining({ method: 'GET' })
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      'http://127.0.0.1:7788/health',
      expect.objectContaining({ method: 'GET' })
    );
    expect(setCoreMock).toHaveBeenCalledWith({ value: 'reachable' });
    stopCoreHealthMonitor();
  });
});
