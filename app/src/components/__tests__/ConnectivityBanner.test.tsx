import { fireEvent, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { renderWithProviders } from '../../test/test-utils';
import ConnectivityBanner from '../ConnectivityBanner';

const restartCoreProcessMock = vi.fn<() => Promise<void>>();

vi.mock('../../services/coreProcessControl', () => ({
  restartCoreProcess: () => restartCoreProcessMock(),
}));

describe('ConnectivityBanner', () => {
  it('does not render when connectivity is healthy', () => {
    renderWithProviders(<ConnectivityBanner />, {
      preloadedState: {
        connectivity: {
          internet: 'online',
          core: 'reachable',
          backend: 'connected',
          lastError: {},
        },
      },
    });

    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });

  it('shows a global offline banner without a core restart action', () => {
    renderWithProviders(<ConnectivityBanner />, {
      preloadedState: {
        connectivity: {
          internet: 'offline',
          core: 'reachable',
          backend: 'connected',
          lastError: { internet: 'navigator offline' },
        },
      },
    });

    expect(screen.getByRole('status')).toHaveTextContent('Your device is offline');
    expect(screen.getByText('navigator offline')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Restart Core/i })).not.toBeInTheDocument();
  });

  it('shows a global core outage banner and can restart the core', async () => {
    restartCoreProcessMock.mockResolvedValueOnce(undefined);

    renderWithProviders(<ConnectivityBanner />, {
      preloadedState: {
        connectivity: {
          internet: 'online',
          core: 'unreachable',
          backend: 'connected',
          lastError: { core: 'ECONNREFUSED' },
        },
      },
    });

    expect(screen.getByRole('status')).toHaveTextContent("The OpenHuman core isn't responding");
    expect(screen.getByText('ECONNREFUSED')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: /Restart Core/i }));

    expect(screen.getByRole('button', { name: /Restarting core/i })).toBeDisabled();
    await waitFor(() => expect(restartCoreProcessMock).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /Restart Core/i })).not.toBeDisabled()
    );
  });

  it('surfaces restart errors in the banner detail line', async () => {
    restartCoreProcessMock.mockRejectedValueOnce(new Error('restart failed'));

    renderWithProviders(<ConnectivityBanner />, {
      preloadedState: {
        connectivity: {
          internet: 'online',
          core: 'unreachable',
          backend: 'connected',
          lastError: { core: 'old error' },
        },
      },
    });

    fireEvent.click(screen.getByRole('button', { name: /Restart Core/i }));

    await waitFor(() => expect(screen.getByText('restart failed')).toBeInTheDocument());
  });
});
