import { beforeEach, describe, expect, it, vi } from 'vitest';

const mockCallCoreRpc = vi.fn();

vi.mock('./coreRpcClient', () => ({
  callCoreRpc: (...args: unknown[]) => mockCallCoreRpc(...args),
}));

// Minimal fixtures -----------------------------------------------------------------

function makeSnapshotResult(overrides: Record<string, unknown> = {}) {
  return {
    auth: { isAuthenticated: true, userId: 'u-1', user: null, profileId: 'p-1' },
    sessionToken: 'tok-abc',
    currentUser: null,
    onboardingCompleted: false,
    analyticsEnabled: true,
    localState: { encryptionKey: null, onboardingTasks: null },
    runtime: { screenIntelligence: {}, localAi: {}, autocomplete: {}, service: {} },
    ...overrides,
  };
}

// Tests ----------------------------------------------------------------------------

describe('coreStateApi.fetchCoreAppSnapshot', () => {
  beforeEach(() => {
    mockCallCoreRpc.mockReset();
  });

  it('calls the correct RPC method', async () => {
    mockCallCoreRpc.mockResolvedValueOnce({ result: makeSnapshotResult() });

    const { fetchCoreAppSnapshot } = await import('./coreStateApi');
    await fetchCoreAppSnapshot();

    expect(mockCallCoreRpc).toHaveBeenCalledWith({ method: 'openhuman.app_state_snapshot' });
  });

  it('returns the inner result from the RPC envelope', async () => {
    const snapshot = makeSnapshotResult({ sessionToken: 'tok-xyz' });
    mockCallCoreRpc.mockResolvedValueOnce({ result: snapshot });

    const { fetchCoreAppSnapshot } = await import('./coreStateApi');
    const out = await fetchCoreAppSnapshot();

    expect(out.sessionToken).toBe('tok-xyz');
    expect(out.auth.isAuthenticated).toBe(true);
    expect(out.auth.userId).toBe('u-1');
  });

  it('propagates rejection from callCoreRpc', async () => {
    mockCallCoreRpc.mockRejectedValueOnce(new Error('snapshot failed'));

    const { fetchCoreAppSnapshot } = await import('./coreStateApi');
    await expect(fetchCoreAppSnapshot()).rejects.toThrow('snapshot failed');
  });
});

describe('coreStateApi.updateCoreLocalState', () => {
  beforeEach(() => {
    mockCallCoreRpc.mockReset();
  });

  it('calls the correct RPC method with params', async () => {
    mockCallCoreRpc.mockResolvedValueOnce({});

    const { updateCoreLocalState } = await import('./coreStateApi');
    const params = { encryptionKey: 'key-123' };
    await updateCoreLocalState(params);

    expect(mockCallCoreRpc).toHaveBeenCalledWith({
      method: 'openhuman.app_state_update_local_state',
      params,
    });
  });

  it('resolves without a return value on success', async () => {
    mockCallCoreRpc.mockResolvedValueOnce({});

    const { updateCoreLocalState } = await import('./coreStateApi');
    const result = await updateCoreLocalState({ encryptionKey: null });
    expect(result).toBeUndefined();
  });

  it('passes null fields correctly to the RPC', async () => {
    mockCallCoreRpc.mockResolvedValueOnce({});

    const { updateCoreLocalState } = await import('./coreStateApi');
    await updateCoreLocalState({ encryptionKey: null, onboardingTasks: null });

    const call = mockCallCoreRpc.mock.calls[0][0] as { params: unknown };
    expect(call.params).toEqual({ encryptionKey: null, onboardingTasks: null });
  });

  it('propagates rejection from callCoreRpc', async () => {
    mockCallCoreRpc.mockRejectedValueOnce(new Error('update rejected'));

    const { updateCoreLocalState } = await import('./coreStateApi');
    await expect(updateCoreLocalState({})).rejects.toThrow('update rejected');
  });
});
