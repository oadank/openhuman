import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useCoreState } from '../../providers/CoreStateProvider';
import { useRefetchSnapshotOnTurnEnd } from '../useRefetchSnapshotOnTurnEnd';

vi.mock('../../providers/CoreStateProvider', () => ({ useCoreState: vi.fn() }));

describe('useRefetchSnapshotOnTurnEnd', () => {
  const mockRefresh = vi.fn();

  beforeEach(() => {
    vi.useFakeTimers();
    mockRefresh.mockResolvedValue(undefined);
    vi.mocked(useCoreState).mockReturnValue({ refresh: mockRefresh } as any);
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it('refreshes the core snapshot after 750ms', async () => {
    const { result } = renderHook(() => useRefetchSnapshotOnTurnEnd());

    act(() => {
      result.current.refetch();
    });

    expect(mockRefresh).not.toHaveBeenCalled();

    await act(async () => {
      vi.advanceTimersByTime(750);
    });

    expect(mockRefresh).toHaveBeenCalledTimes(1);
  });

  it('three rapid finalize events → one refresh call', async () => {
    const { result } = renderHook(() => useRefetchSnapshotOnTurnEnd());

    act(() => {
      result.current.refetch();
      vi.advanceTimersByTime(300);
      result.current.refetch();
      vi.advanceTimersByTime(300);
      result.current.refetch();
    });

    expect(mockRefresh).not.toHaveBeenCalled();

    await act(async () => {
      vi.advanceTimersByTime(750);
    });

    expect(mockRefresh).toHaveBeenCalledTimes(1);
  });

  it('two sequential finalize events trigger two refreshes', async () => {
    const { result } = renderHook(() => useRefetchSnapshotOnTurnEnd());

    // First refetch
    act(() => {
      result.current.refetch();
    });
    await act(async () => {
      vi.advanceTimersByTime(750);
    });
    expect(mockRefresh).toHaveBeenCalledTimes(1);

    // Second refetch
    act(() => {
      result.current.refetch();
    });
    await act(async () => {
      vi.advanceTimersByTime(750);
    });
    expect(mockRefresh).toHaveBeenCalledTimes(2);
  });

  it('clears the pending debounce timer on unmount so refresh never fires', async () => {
    const { result, unmount } = renderHook(() => useRefetchSnapshotOnTurnEnd());

    act(() => {
      result.current.refetch();
    });

    unmount();

    await act(async () => {
      vi.advanceTimersByTime(750);
    });

    expect(mockRefresh).not.toHaveBeenCalled();
  });
});
