import { useCallback, useEffect, useRef } from 'react';

import { useCoreState } from '../providers/CoreStateProvider';

/**
 * Hook to refetch the authoritative local core snapshot after a chat turn
 * finishes. Updates the global snapshot in CoreStateProvider.
 *
 * Includes a 750ms debounce to collapse multiple rapid turn-finalized events.
 */
export function useRefetchSnapshotOnTurnEnd() {
  const { refresh } = useCoreState();
  const debounceTimerRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (debounceTimerRef.current !== null) {
        window.clearTimeout(debounceTimerRef.current);
        debounceTimerRef.current = null;
      }
    };
  }, []);

  const refetch = useCallback(() => {
    if (debounceTimerRef.current !== null) {
      window.clearTimeout(debounceTimerRef.current);
    }

    debounceTimerRef.current = window.setTimeout(() => {
      debounceTimerRef.current = null;

      // Fire-and-forget on a microtask
      void (async () => {
        try {
          await refresh();
        } catch (error) {
          console.warn('[useRefetchSnapshotOnTurnEnd] failed to refetch core state:', error);
        }
      })();
    }, 750);
  }, [refresh]);

  return { refetch };
}
