import { useT } from '../../lib/i18n/I18nContext';
import { useAppSelector } from '../../store/hooks';

function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}K`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

const TokenUsagePill = () => {
  const { t } = useT();
  const sessionTokens = useAppSelector(state => state.chatRuntime.sessionTokenUsage);

  const totalTokens = sessionTokens.inputTokens + sessionTokens.outputTokens;

  if (totalTokens <= 0) return null;

  return (
    <div className="flex items-center gap-1.5 text-[11px] leading-none">
      <span
        className="inline-flex items-center gap-1 rounded-full bg-stone-100 dark:bg-neutral-800 px-2 py-1 font-mono text-stone-600 dark:text-neutral-300 ring-1 ring-stone-200/60 dark:ring-neutral-700"
        title={t('token.sessionTokens')
          .replace('{in}', sessionTokens.inputTokens.toLocaleString())
          .replace('{out}', sessionTokens.outputTokens.toLocaleString())
          .replace('{turns}', String(sessionTokens.turns))}>
        <svg className="h-3 w-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M13 10V3L4 14h7v7l9-11h-7z"
          />
        </svg>
        {formatTokens(totalTokens)}
      </span>
    </div>
  );
};

export default TokenUsagePill;
