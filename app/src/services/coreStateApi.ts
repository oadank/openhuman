import type { User } from '../types/api';
import type { AccessibilityStatus } from '../utils/tauriCommands/accessibility';
import type { AutocompleteStatus } from '../utils/tauriCommands/autocomplete';
import type { LocalAiStatus } from '../utils/tauriCommands/localAi';
import type { ServiceStatus } from '../utils/tauriCommands/service';
import { callCoreRpc } from './coreRpcClient';

export interface OnboardingTasks {
  accessibilityPermissionGranted: boolean;
  localModelConsentGiven: boolean;
  localModelDownloadStarted: boolean;
  enabledTools: string[];
  connectedSources: string[];
  updatedAtMs?: number;
}

export interface UpdateCoreLocalStateParams {
  encryptionKey?: string | null;
  onboardingTasks?: OnboardingTasks | null;
}

interface AppStateSnapshotResult {
  auth: {
    isAuthenticated: boolean;
    userId: string | null;
    user: unknown | null;
    profileId: string | null;
  };
  sessionToken: string | null;
  currentUser: User | null;
  onboardingCompleted: boolean;
  chatOnboardingCompleted: boolean;
  analyticsEnabled: boolean;
  /**
   * Mirror of `Config::meet.auto_orchestrator_handoff` (#1299). Older
   * core builds may omit the field on the wire — `fetchCoreAppSnapshot`
   * normalises the missing case to `false` before returning so callers
   * never observe `undefined` here.
   */
  meetAutoOrchestratorHandoff?: boolean;
  localState: { encryptionKey?: string | null; onboardingTasks?: OnboardingTasks | null };
  runtime: {
    screenIntelligence: AccessibilityStatus;
    localAi: LocalAiStatus;
    autocomplete: AutocompleteStatus;
    service: ServiceStatus;
  };
}

export const fetchCoreAppSnapshot = async (): Promise<AppStateSnapshotResult> => {
  const response = await callCoreRpc<{ result: AppStateSnapshotResult }>({
    method: 'openhuman.app_state_snapshot',
  });
  // Normalise the optional #1299 field at the API boundary so older core
  // builds without `meetAutoOrchestratorHandoff` still surface the
  // privacy-conservative `false` to callers (e.g. CoreStateProvider).
  return {
    ...response.result,
    meetAutoOrchestratorHandoff: response.result.meetAutoOrchestratorHandoff ?? false,
  };
};

export const updateCoreLocalState = async (params: UpdateCoreLocalStateParams): Promise<void> => {
  await callCoreRpc({ method: 'openhuman.app_state_update_local_state', params });
};
