use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use log::{debug, warn};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tempfile::NamedTempFile;

use crate::openhuman::autocomplete::AutocompleteStatus;
use crate::openhuman::config::rpc as config_rpc;
use crate::openhuman::config::Config;
use crate::openhuman::credentials::responses::AuthStateResponse;
use crate::openhuman::inference::LocalAiStatus;
use crate::openhuman::screen_intelligence::AccessibilityStatus;
use crate::openhuman::service::{ServiceState, ServiceStatus};
use crate::rpc::RpcOutcome;

const LOG_PREFIX: &str = "[app_state]";
const APP_STATE_FILENAME: &str = "app-state.json";
static APP_STATE_FILE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static CURRENT_USER_CACHE: Lazy<Mutex<Option<CachedCurrentUser>>> = Lazy::new(|| Mutex::new(None));

#[derive(Debug, Clone)]
struct CachedCurrentUser {
    user: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredOnboardingTasks {
    #[serde(default)]
    pub accessibility_permission_granted: bool,
    #[serde(default)]
    pub local_model_consent_given: bool,
    #[serde(default)]
    pub local_model_download_started: bool,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub connected_sources: Vec<String>,
    #[serde(default)]
    pub updated_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredAppState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onboarding_tasks: Option<StoredOnboardingTasks>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStateSnapshot {
    pub auth: crate::openhuman::credentials::responses::AuthStateResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_user: Option<Value>,
    pub onboarding_completed: bool,
    /// Whether the chat-based welcome-agent flow has completed. Sourced
    /// from [`Config::chat_onboarding_completed`]. The React app hides
    /// the bottom tab bar, thread sidebar, and account rail while this is
    /// `false` (and `onboarding_completed` is `true`) so the user stays
    /// with the welcome agent until it calls
    /// `complete_onboarding(action="complete")`.
    pub chat_onboarding_completed: bool,
    pub analytics_enabled: bool,
    /// Mirror of `Config::meet.auto_orchestrator_handoff` — gates whether
    /// ending a Google Meet call hands the transcript to the orchestrator
    /// agent for proactive follow-up actions. Default `false`. See
    /// issue #1299.
    pub meet_auto_orchestrator_handoff: bool,
    pub local_state: StoredAppState,
    pub runtime: RuntimeSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub screen_intelligence: AccessibilityStatus,
    pub local_ai: LocalAiStatus,
    pub autocomplete: AutocompleteStatus,
    pub service: ServiceStatus,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredAppStatePatch {
    #[serde(default)]
    pub encryption_key: Option<Option<String>>,
    #[serde(default)]
    pub onboarding_tasks: Option<Option<StoredOnboardingTasks>>,
}

fn app_state_path(config: &Config) -> Result<PathBuf, String> {
    let state_dir = config.workspace_dir.join("state");
    fs::create_dir_all(&state_dir).map_err(|e| {
        format!(
            "failed to create workspace state dir {}: {e}",
            state_dir.display()
        )
    })?;
    Ok(state_dir.join(APP_STATE_FILENAME))
}

fn corrupted_app_state_path(path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or(0);
    path.with_extension(format!("json.corrupted.{timestamp}"))
}

fn quarantine_corrupted_app_state(path: &Path, reason: &str) {
    let quarantine_path = corrupted_app_state_path(path);
    warn!(
        "{LOG_PREFIX} quarantining corrupted app state {} -> {} ({reason})",
        path.display(),
        quarantine_path.display()
    );

    if let Err(rename_error) = fs::rename(path, &quarantine_path) {
        warn!(
            "{LOG_PREFIX} failed to quarantine {} via rename: {}",
            path.display(),
            rename_error
        );
        if let Err(remove_error) = fs::remove_file(path) {
            warn!(
                "{LOG_PREFIX} failed to remove unreadable app state {}: {}",
                path.display(),
                remove_error
            );
        }
    }
}

fn load_stored_app_state_unlocked(config: &Config) -> Result<StoredAppState, String> {
    let path = app_state_path(config)?;
    if !path.exists() {
        return Ok(StoredAppState::default());
    }

    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => {
            warn!(
                "{LOG_PREFIX} failed to read {}; falling back to defaults: {}",
                path.display(),
                error
            );
            quarantine_corrupted_app_state(&path, &error.to_string());
            return Ok(StoredAppState::default());
        }
    };

    match serde_json::from_str::<StoredAppState>(&raw) {
        Ok(state) => Ok(state),
        Err(error) => {
            warn!(
                "{LOG_PREFIX} failed to parse {}; falling back to defaults: {}",
                path.display(),
                error
            );
            quarantine_corrupted_app_state(&path, &error.to_string());
            Ok(StoredAppState::default())
        }
    }
}

pub(crate) fn load_stored_app_state(config: &Config) -> Result<StoredAppState, String> {
    let _guard = APP_STATE_FILE_LOCK.lock();
    load_stored_app_state_unlocked(config)
}

fn sync_parent_dir(path: &Path) -> Result<(), String> {
    // Directory fsync is a POSIX-only durability guarantee — on Unix we
    // open the parent dir and call `sync_all()` so the rename of the
    // temp file into place is persisted even if the host crashes before
    // the next buffer flush. On Windows, opening a directory as a
    // regular file requires `FILE_FLAG_BACKUP_SEMANTICS` which
    // `std::fs::File::open` does not set, so the call fails with
    // "Access is denied. (os error 5)". Since Windows uses a different
    // durability model (and `NamedTempFile::persist` issues an atomic
    // MoveFileEx which is already durable enough for our config files),
    // we skip the fsync entirely on non-Unix and return Ok. Mirrors the
    // existing `sync_directory` guard in `config/schema/load.rs`.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("failed to sync directory {}: {e}", parent.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn save_stored_app_state_unlocked(config: &Config, state: &StoredAppState) -> Result<(), String> {
    let path = app_state_path(config)?;
    let payload = serde_json::to_string_pretty(state)
        .map_err(|e| format!("failed to serialize app state: {e}"))?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("failed to resolve parent dir for {}", path.display()))?;
    let mut temp_file = NamedTempFile::new_in(parent)
        .map_err(|e| format!("failed to create temp file in {}: {e}", parent.display()))?;
    temp_file
        .write_all(payload.as_bytes())
        .map_err(|e| format!("failed to write temp app state for {}: {e}", path.display()))?;
    temp_file
        .as_file_mut()
        .sync_all()
        .map_err(|e| format!("failed to sync temp app state for {}: {e}", path.display()))?;
    sync_parent_dir(&path)?;
    temp_file.persist(&path).map_err(|e| {
        format!(
            "failed to persist app state {}: {}",
            path.display(),
            e.error
        )
    })?;
    sync_parent_dir(&path)?;
    Ok(())
}

fn save_stored_app_state(config: &Config, state: &StoredAppState) -> Result<(), String> {
    let _guard = APP_STATE_FILE_LOCK.lock();
    save_stored_app_state_unlocked(config, state)
}

fn local_current_user() -> Value {
    json!({
        "user_id": null,
        "email": null,
        "display_name": "Local User",
        "source": "local",
    })
}

fn cache_local_current_user(user: &Value) {
    let mut cache = CURRENT_USER_CACHE.lock();
    *cache = Some(CachedCurrentUser { user: user.clone() });
}

/// Synchronous, network-free peek at the cached `auth_get_me` response,
/// returning only the identifying fields the prompt layer is allowed to
/// embed (`id`, `name`, `email`). Tokens stay locked behind the JWT
/// helpers — never returned through this path. See issue #926.
///
/// Returns `None` when no `auth_get_me` call has populated the cache
/// yet (CLI-only flows, fresh installs, signed-out sessions). The
/// cache TTL is **ignored** here intentionally — for prompt rendering
/// a slightly stale identity is fine; the freshness check only
/// matters for the snapshot RPC that fronts the React shell.
pub fn peek_cached_current_user_identity() -> Option<crate::openhuman::agent::prompts::UserIdentity>
{
    let cache = CURRENT_USER_CACHE.lock();
    let entry = cache.as_ref()?;
    let user = entry.user.as_object()?;

    let pluck = |key: &str| -> Option<String> {
        user.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let id = pluck("id")
        .or_else(|| pluck("user_id"))
        .or_else(|| pluck("userId"));
    let name = pluck("name")
        .or_else(|| pluck("displayName"))
        .or_else(|| pluck("display_name"))
        .or_else(|| pluck("full_name"))
        .or_else(|| pluck("fullName"));
    let email = pluck("email");

    let identity = crate::openhuman::agent::prompts::UserIdentity { id, name, email };
    if identity.is_empty() {
        None
    } else {
        Some(identity)
    }
}

async fn build_runtime_snapshot(config: &Config) -> RuntimeSnapshot {
    let screen_intelligence = {
        let _ = crate::openhuman::screen_intelligence::global_engine()
            .apply_config(config.screen_intelligence.clone())
            .await;
        crate::openhuman::screen_intelligence::global_engine()
            .status()
            .await
    };

    let local_ai = match crate::openhuman::inference::rpc::inference_status(config).await {
        Ok(outcome) => outcome.value,
        Err(error) => {
            warn!("{LOG_PREFIX} local_ai status failed during snapshot: {error}");
            crate::openhuman::inference::LocalAiStatus::disabled(config)
        }
    };

    let autocomplete = crate::openhuman::autocomplete::global_engine()
        .status()
        .await;

    let service = match crate::openhuman::service::status(config) {
        Ok(status) => status,
        Err(error) => {
            let message = error.to_string();
            warn!("{LOG_PREFIX} service status failed during snapshot: {message}");
            ServiceStatus {
                state: ServiceState::Unknown(message.clone()),
                unit_path: None,
                label: "OpenHuman".to_string(),
                details: Some(message),
            }
        }
    };

    RuntimeSnapshot {
        screen_intelligence,
        local_ai,
        autocomplete,
        service,
    }
}

pub async fn snapshot() -> Result<RpcOutcome<AppStateSnapshot>, String> {
    let config = config_rpc::load_config_with_timeout().await?;
    let current_user = Some(local_current_user());
    if let Some(user) = current_user.as_ref() {
        cache_local_current_user(user);
    }
    let auth = AuthStateResponse {
        is_authenticated: true,
        user_id: None,
        user: current_user.clone(),
        profile_id: None,
    };
    let local_state = load_stored_app_state(&config)?;
    let runtime = build_runtime_snapshot(&config).await;

    debug!(
        "{LOG_PREFIX} snapshot auth={} onboarding={} chat_onboarding={} analytics={} meet_handoff={} si_active={} local_ai_state={} autocomplete_phase={} service_state={:?}",
        auth.is_authenticated,
        config.onboarding_completed,
        config.chat_onboarding_completed,
        config.observability.analytics_enabled,
        config.meet.auto_orchestrator_handoff,
        runtime.screen_intelligence.session.active,
        runtime.local_ai.state,
        runtime.autocomplete.phase,
        runtime.service.state
    );

    Ok(RpcOutcome::new(
        AppStateSnapshot {
            auth,
            session_token: None,
            current_user,
            onboarding_completed: config.onboarding_completed,
            chat_onboarding_completed: config.chat_onboarding_completed,
            analytics_enabled: config.observability.analytics_enabled,
            meet_auto_orchestrator_handoff: config.meet.auto_orchestrator_handoff,
            local_state,
            runtime,
        },
        vec!["core app state snapshot fetched".to_string()],
    ))
}

pub async fn update_local_state(
    patch: StoredAppStatePatch,
) -> Result<RpcOutcome<StoredAppState>, String> {
    let config = config_rpc::load_config_with_timeout().await?;
    let _guard = APP_STATE_FILE_LOCK.lock();
    let mut current = load_stored_app_state_unlocked(&config)?;

    if let Some(encryption_key) = patch.encryption_key {
        current.encryption_key = encryption_key.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
    }

    if let Some(onboarding_tasks) = patch.onboarding_tasks {
        current.onboarding_tasks = onboarding_tasks;
    }

    save_stored_app_state_unlocked(&config, &current)?;

    debug!(
        "{LOG_PREFIX} local state updated encryption_key={} onboarding_tasks={}",
        current.encryption_key.is_some(),
        current.onboarding_tasks.is_some()
    );

    Ok(RpcOutcome::new(
        current,
        vec!["core local app state updated".to_string()],
    ))
}
