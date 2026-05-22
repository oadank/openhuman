import { useState } from 'react';

import { useT } from '../lib/i18n/I18nContext';
import { restartCoreProcess } from '../services/coreProcessControl';
import { selectBlockingState, selectConnectivityErrors } from '../store/connectivitySelectors';
import { useAppSelector } from '../store/hooks';

const ConnectivityBanner = () => {
  const { t } = useT();
  const blocking = useAppSelector(selectBlockingState);
  const errors = useAppSelector(selectConnectivityErrors);
  const [isRestarting, setIsRestarting] = useState(false);
  const [restartError, setRestartError] = useState<string | null>(null);

  if (blocking === 'ok' || blocking === 'backend-only') {
    return null;
  }

  const isCoreDown = blocking === 'core-unreachable';
  const message = isCoreDown ? t('home.statusCoreUnreachable') : t('home.statusInternetOffline');
  const detail = isCoreDown ? restartError || errors.core : errors.internet;

  const handleRestartCore = async () => {
    setIsRestarting(true);
    setRestartError(null);
    try {
      await restartCoreProcess();
    } catch (err) {
      setRestartError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsRestarting(false);
    }
  };

  return (
    <div
      role="status"
      aria-live="polite"
      className={`relative z-30 border-b px-4 py-2 ${
        isCoreDown
          ? 'border-amber-300 bg-amber-50 text-amber-950 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-100'
          : 'border-coral-300 bg-coral-50 text-coral-950 dark:border-coral-500/40 dark:bg-coral-500/10 dark:text-coral-100'
      }`}>
      <div className="mx-auto flex max-w-5xl flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <p className="text-sm font-medium leading-snug">{message}</p>
          {detail && <p className="mt-0.5 truncate text-xs opacity-80">{detail}</p>}
        </div>
        {isCoreDown && (
          <button
            type="button"
            onClick={handleRestartCore}
            disabled={isRestarting}
            className="shrink-0 rounded-md border border-amber-400/70 bg-white/70 px-3 py-1.5 text-xs font-medium text-amber-950 hover:bg-white disabled:opacity-60 dark:border-amber-400/40 dark:bg-neutral-950/40 dark:text-amber-100 dark:hover:bg-neutral-950/70">
            {isRestarting ? t('home.restartingCore') : t('home.restartCore')}
          </button>
        )}
      </div>
    </div>
  );
};

export default ConnectivityBanner;
