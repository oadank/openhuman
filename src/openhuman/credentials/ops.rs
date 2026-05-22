//! JSON-RPC / CLI controller surface for credentials and app session auth.

use serde_json::json;

use crate::openhuman::config::Config;
use crate::openhuman::credentials::profiles::AuthProfileKind;
use crate::openhuman::credentials::profiles::TokenSet;
use crate::openhuman::credentials::responses::AuthStateResponse;
use crate::openhuman::security::SecretStore;
use crate::rpc::RpcOutcome;

use super::{AuthService, DEFAULT_AUTH_PROFILE_NAME};
use crate::openhuman::config::default_root_openhuman_dir;

/// Start all login-gated background services (local AI, voice, screen
/// intelligence, autocomplete).  Called both from the initial boot path
/// (when an existing session is detected) and from `store_session()` on
/// fresh login.
pub async fn start_login_gated_services(config: &Config) {
    // 1. Local AI (Ollama, whisper, embeddings)
    if config.local_ai.runtime_enabled {
        let service = crate::openhuman::inference::local::global(config);
        service.bootstrap(config).await;
        log::info!("[services] local AI bootstrapped after login");
    }

    // 2. Voice server (records + transcribes via hotkey)
    crate::openhuman::voice::server::start_if_enabled(config).await;

    // 3. Dictation hotkey listener (only when voice server is NOT auto-started,
    //    since the voice server owns the single rdev listener on macOS)
    if !config.voice_server.auto_start {
        crate::openhuman::voice::dictation_listener::start_if_enabled(config).await;
    }

    // 4. Screen intelligence (capture + vision analysis)
    crate::openhuman::screen_intelligence::server::start_if_enabled(config).await;

    // 5. Autocomplete (text suggestions + Swift overlay helper)
    crate::openhuman::autocomplete::start_if_enabled(config).await;

    log::info!("[services] all login-gated services started");
}

/// Stop all login-gated background services.  Called from `clear_session()`
/// on logout so orphan processes don't consume resources.
pub async fn stop_login_gated_services(config: &Config) {
    // 1. Autocomplete — stop engine + Swift overlay helper.
    {
        let engine = crate::openhuman::autocomplete::global_engine();
        let status = engine.status().await;
        if status.running {
            engine.stop(None).await;
            log::info!("[services] autocomplete engine stopped on logout");
        }
    }

    // 2. Voice server
    if let Some(server) = crate::openhuman::voice::server::try_global_server() {
        server.stop().await;
        log::info!("[services] voice server stopped on logout");
    }

    // 3. Screen intelligence server
    if let Some(server) = crate::openhuman::screen_intelligence::server::try_global_server() {
        server.stop().await;
        log::info!("[services] screen intelligence server stopped on logout");
    }

    // 4. Local AI — reset state to idle. We don't kill the Ollama process
    //    (it may be serving other clients or mid-download), but we clear
    //    the internal state so it re-bootstraps on next login.
    if config.local_ai.runtime_enabled {
        let service = crate::openhuman::inference::local::global(config);
        service.reset_to_idle(config);
        log::info!("[services] local AI reset to idle on logout");
    }

    // 5. Dictation listener — abort the hotkey forwarder task so it doesn't
    //    accumulate duplicate rdev listeners across logout → login cycles.
    crate::openhuman::voice::dictation_listener::stop();

    log::info!("[services] all login-gated services stopped");
}

fn secret_store_for_config(config: &Config) -> SecretStore {
    let data_dir = config
        .config_path
        .parent()
        .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
    SecretStore::new(&data_dir, true)
}

fn profile_name_or_default(value: Option<&str>) -> &str {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_AUTH_PROFILE_NAME)
}

fn parse_fields_value(
    input: Option<serde_json::Value>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let Some(value) = input else {
        return Ok(std::collections::HashMap::new());
    };

    let Some(map) = value.as_object() else {
        return Err("fields must be a JSON object".to_string());
    };

    let mut out = std::collections::HashMap::new();
    for (key, raw) in map {
        if key.trim().is_empty() {
            return Err("fields cannot contain empty keys".to_string());
        }
        let rendered = match raw {
            serde_json::Value::Null => String::new(),
            serde_json::Value::String(s) => s.clone(),
            _ => raw.to_string(),
        };
        out.insert(key.clone(), rendered);
    }

    Ok(out)
}

fn profile_kind_label(kind: AuthProfileKind) -> String {
    match kind {
        AuthProfileKind::OAuth => "oauth".to_string(),
        AuthProfileKind::Token => "token".to_string(),
    }
}

fn summarize_auth_profile(
    profile: &crate::openhuman::credentials::profiles::AuthProfile,
) -> super::responses::AuthProfileSummary {
    let mut metadata_keys = profile
        .metadata
        .keys()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();
    metadata_keys.sort();

    super::responses::AuthProfileSummary {
        id: profile.id.clone(),
        provider: profile.provider.clone(),
        profile_name: profile.profile_name.clone(),
        kind: profile_kind_label(profile.kind),
        account_id: profile.account_id.clone(),
        workspace_id: profile.workspace_id.clone(),
        metadata_keys,
        updated_at: profile.updated_at.to_rfc3339(),
        has_token: profile.token.as_ref().is_some_and(|v| !v.trim().is_empty()),
        has_token_set: profile
            .token_set
            .as_ref()
            .map(|TokenSet { access_token, .. }| !access_token.trim().is_empty())
            .unwrap_or(false),
    }
}

pub async fn encrypt_secret(
    config: &Config,
    plaintext: &str,
) -> Result<RpcOutcome<String>, String> {
    let store = secret_store_for_config(config);
    let ciphertext = store.encrypt(plaintext).map_err(|e| e.to_string())?;
    Ok(RpcOutcome::single_log(ciphertext, "secret encrypted"))
}

pub async fn decrypt_secret(
    config: &Config,
    ciphertext: &str,
) -> Result<RpcOutcome<String>, String> {
    let store = secret_store_for_config(config);
    let plaintext = store.decrypt(ciphertext).map_err(|e| e.to_string())?;
    Ok(RpcOutcome::single_log(plaintext, "secret decrypted"))
}

pub async fn clear_session(config: &Config) -> Result<RpcOutcome<serde_json::Value>, String> {
    // Clear the active user marker so subsequent config loads fall back to the
    // default (unauthenticated) openhuman directory.
    if let Ok(root_dir) = default_root_openhuman_dir() {
        if let Err(e) = crate::openhuman::config::clear_active_user(&root_dir) {
            tracing::warn!(error = %e, "failed to clear active_user.toml on logout");
        }
    }

    // Stop all login-gated services (voice, autocomplete, screen
    // intelligence, local AI) so they don't run as orphan processes after
    // logout, consuming RAM/CPU with no user context to operate against.
    stop_login_gated_services(config).await;

    // Tear down the subconscious engine + heartbeat loop. Without this the
    // cached engine would keep pointing at the previous user's workspace_dir
    // and the heartbeat task would leak, ticking against the wrong DB when a
    // different user signs in to the same sidecar process.
    crate::openhuman::subconscious::global::reset_engine_for_user_switch().await;

    Ok(RpcOutcome::single_log(
        json!({ "removed": false }),
        "local session state cleared",
    ))
}

pub async fn auth_get_state(
    _config: &Config,
) -> Result<RpcOutcome<super::responses::AuthStateResponse>, String> {
    let state = AuthStateResponse {
        is_authenticated: true,
        user_id: None,
        user: Some(json!({
            "user_id": null,
            "email": null,
            "display_name": "Local User",
            "source": "local",
        })),
        profile_id: None,
    };
    Ok(RpcOutcome::single_log(state, "session state fetched"))
}

pub async fn store_provider_credentials(
    config: &Config,
    provider: &str,
    profile: Option<&str>,
    token: Option<String>,
    fields: Option<serde_json::Value>,
    set_active: Option<bool>,
) -> Result<RpcOutcome<super::responses::AuthProfileSummary>, String> {
    let provider = provider.trim().to_string();
    if provider.is_empty() {
        return Err("provider is required".to_string());
    }

    let profile_name = profile_name_or_default(profile);
    let mut metadata = parse_fields_value(fields)?;
    let token = token
        .as_ref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| metadata.get("token").cloned())
        .or_else(|| metadata.get("api_key").cloned())
        .unwrap_or_default();
    if token.is_empty() && metadata.is_empty() {
        return Err("provide at least one credential via token or fields".to_string());
    }
    metadata.remove("token");

    let auth = AuthService::from_config(config);
    let stored = auth
        .store_provider_token(
            &provider,
            profile_name,
            &token,
            metadata,
            set_active.unwrap_or(true),
        )
        .map_err(|e| e.to_string())?;
    Ok(RpcOutcome::single_log(
        summarize_auth_profile(&stored),
        "provider credentials stored",
    ))
}

pub async fn remove_provider_credentials(
    config: &Config,
    provider: &str,
    profile: Option<&str>,
) -> Result<RpcOutcome<serde_json::Value>, String> {
    let profile_name = profile_name_or_default(profile);
    let auth = AuthService::from_config(config);
    let removed = auth
        .remove_profile(provider, profile_name)
        .map_err(|e| e.to_string())?;
    Ok(RpcOutcome::single_log(
        json!({
            "removed": removed,
            "provider": provider,
            "profile": profile_name,
        }),
        "provider credentials removed",
    ))
}

pub async fn list_provider_credentials(
    config: &Config,
    provider_filter: Option<String>,
) -> Result<RpcOutcome<Vec<super::responses::AuthProfileSummary>>, String> {
    let auth = AuthService::from_config(config);
    let profiles = auth.load_profiles().map_err(|e| e.to_string())?;
    let mut items = profiles
        .profiles
        .values()
        .filter(|profile| {
            provider_filter
                .as_ref()
                .is_none_or(|provider| profile.provider == *provider)
        })
        .map(summarize_auth_profile)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.profile_name.cmp(&b.profile_name))
    });

    Ok(RpcOutcome::single_log(items, "provider credentials listed"))
}

/// List credentials whose provider key starts with `prefix`.
///
/// Pure prefix variant of [`list_provider_credentials`] for namespaces
/// that group multiple providers under a common stem (e.g.
/// `"channel:"` covers `channel:telegram:managed_dm`,
/// `channel:slack:bot_token`, …). The exact-match filter on
/// `list_provider_credentials` cannot express this without enumerating
/// every concrete provider key up front.
pub async fn list_provider_credentials_by_prefix(
    config: &Config,
    prefix: &str,
) -> Result<Vec<super::responses::AuthProfileSummary>, String> {
    let auth = AuthService::from_config(config);
    let profiles = auth.load_profiles().map_err(|e| e.to_string())?;
    let mut items = profiles
        .profiles
        .values()
        .filter(|profile| profile.provider.starts_with(prefix))
        .map(summarize_auth_profile)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.profile_name.cmp(&b.profile_name))
    });
    Ok(items)
}

/// Provider slot for the user-provided Composio API key when running in
/// direct mode (BYO key).
///
/// Stored via the same
/// [`super::profiles::AuthProfilesStore`] backend (encrypted on disk
/// when `secrets.encrypt = true`).
pub const COMPOSIO_DIRECT_PROVIDER: &str = "composio-direct";

/// Persist the user-provided Composio API key to the encrypted credential
/// store under [`COMPOSIO_DIRECT_PROVIDER`].
///
/// **Never log the API key itself** — the debug line below records only
/// length and a length-of-stored marker. This honours the CLAUDE.md
/// debug-logging rule (`Never log secrets … redact or omit`).
pub async fn store_composio_api_key(
    config: &Config,
    api_key: &str,
) -> Result<RpcOutcome<serde_json::Value>, String> {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        return Err("composio api_key must not be empty".to_string());
    }
    tracing::debug!(
        len = trimmed.len(),
        "[composio-direct] storing api key (redacted)"
    );
    let auth = AuthService::from_config(config);
    auth.store_provider_token(
        COMPOSIO_DIRECT_PROVIDER,
        DEFAULT_AUTH_PROFILE_NAME,
        trimmed,
        std::collections::HashMap::new(),
        true,
    )
    .map_err(|e| e.to_string())?;

    Ok(RpcOutcome::single_log(
        json!({ "stored": true, "provider": COMPOSIO_DIRECT_PROVIDER }),
        "composio direct api key stored",
    ))
}

/// Read the user-provided Composio API key from the encrypted credential
/// store. Returns `Ok(None)` when no key has been stored yet.
///
/// Used by [`crate::openhuman::composio::client::create_composio_client`]
/// to decide whether direct mode can actually be activated.
pub fn get_composio_api_key(config: &Config) -> Result<Option<String>, String> {
    let auth = AuthService::from_config(config);
    let key = auth
        .get_provider_bearer_token(COMPOSIO_DIRECT_PROVIDER, None)
        .map_err(|e| e.to_string())?;
    Ok(key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()))
}

/// RPC wrapper around [`store_composio_api_key`] — accepts plain string
/// for symmetry with `store_provider_credentials` while only persisting
/// the trimmed value.
pub async fn rpc_store_composio_api_key(
    config: &Config,
    api_key: &str,
) -> Result<RpcOutcome<serde_json::Value>, String> {
    store_composio_api_key(config, api_key).await
}

/// Remove the stored Composio direct-mode API key. Used when the user
/// switches back to backend mode and explicitly clears their key.
pub async fn clear_composio_api_key(
    config: &Config,
) -> Result<RpcOutcome<serde_json::Value>, String> {
    tracing::debug!("[composio-direct] clearing stored api key");
    let auth = AuthService::from_config(config);
    let removed = auth
        .remove_profile(COMPOSIO_DIRECT_PROVIDER, DEFAULT_AUTH_PROFILE_NAME)
        .map_err(|e| e.to_string())?;
    Ok(RpcOutcome::single_log(
        json!({ "removed": removed }),
        "composio direct api key cleared",
    ))
}

// ── ngrok authtoken (Composio webhook receiver tunnel) ────────────────

/// Provider key the ngrok tunnel authtoken is stored under in the
/// encrypted credential store. Used by
/// [`crate::openhuman::composio::webhook_receiver::tunnel`] to bring up
/// the ngrok session at app boot when direct-mode triggers are wanted.
///
/// Independent from [`COMPOSIO_DIRECT_PROVIDER`] because users may run
/// in either mode but still want ngrok set up: backend-mode users do
/// not use the tunnel at all; direct-mode users use it to receive
/// Composio webhook deliveries.
pub const NGROK_AUTHTOKEN_PROVIDER: &str = "ngrok-tunnel";

/// Persist the user-provided ngrok authtoken to the encrypted
/// credential store under [`NGROK_AUTHTOKEN_PROVIDER`].
///
/// **Never log the authtoken itself** — the debug line below records
/// only length, never the token. Same redaction discipline as
/// [`store_composio_api_key`].
pub async fn store_ngrok_authtoken(
    config: &Config,
    authtoken: &str,
) -> Result<RpcOutcome<serde_json::Value>, String> {
    let trimmed = authtoken.trim();
    if trimmed.is_empty() {
        return Err("ngrok authtoken must not be empty".to_string());
    }
    tracing::debug!(
        len = trimmed.len(),
        "[ngrok-tunnel] storing authtoken (redacted)"
    );
    let auth = AuthService::from_config(config);
    auth.store_provider_token(
        NGROK_AUTHTOKEN_PROVIDER,
        DEFAULT_AUTH_PROFILE_NAME,
        trimmed,
        std::collections::HashMap::new(),
        true,
    )
    .map_err(|e| e.to_string())?;

    Ok(RpcOutcome::single_log(
        json!({ "stored": true, "provider": NGROK_AUTHTOKEN_PROVIDER }),
        "ngrok authtoken stored",
    ))
}

/// Read the user-provided ngrok authtoken from the encrypted credential
/// store. Returns `Ok(None)` when no token has been stored.
///
/// Used by
/// [`crate::openhuman::composio::webhook_receiver::tunnel::connect`]
/// to gate tunnel startup — no token → receiver stays idle (trigger
/// writes surface the existing gate error).
pub fn get_ngrok_authtoken(config: &Config) -> Result<Option<String>, String> {
    let auth = AuthService::from_config(config);
    let token = auth
        .get_provider_bearer_token(NGROK_AUTHTOKEN_PROVIDER, None)
        .map_err(|e| e.to_string())?;
    Ok(token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty()))
}

/// Remove the stored ngrok authtoken. Used when the user wants to
/// disable the tunnel without uninstalling the app.
pub async fn clear_ngrok_authtoken(
    config: &Config,
) -> Result<RpcOutcome<serde_json::Value>, String> {
    tracing::debug!("[ngrok-tunnel] clearing stored authtoken");
    let auth = AuthService::from_config(config);
    let removed = auth
        .remove_profile(NGROK_AUTHTOKEN_PROVIDER, DEFAULT_AUTH_PROFILE_NAME)
        .map_err(|e| e.to_string())?;
    Ok(RpcOutcome::single_log(
        json!({ "removed": removed }),
        "ngrok authtoken cleared",
    ))
}

// ── Composio webhook subscription secret ──────────────────────────────

/// Provider key for the Composio webhook signing secret returned at
/// subscription creation. Stored once on first
/// [`crate::openhuman::composio::webhook_receiver::subscription::ensure_subscription`]
/// call; reused on every subsequent restart to verify inbound deliveries.
///
/// The secret is returned by Composio ONLY at subscription creation
/// time — losing it requires rotating by deleting and recreating the
/// subscription (so any third party who got hold of an old secret can
/// no longer forge events).
pub const COMPOSIO_WEBHOOK_SECRET_PROVIDER: &str = "composio-webhook";

/// Persist the per-subscription HMAC signing secret.
///
/// Caller is `ensure_subscription` — never expose a setter to the
/// frontend, since the secret comes from Composio's response, not from
/// the user.
pub fn store_composio_webhook_secret(config: &Config, secret: &str) -> Result<(), String> {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return Err("composio webhook secret must not be empty".to_string());
    }
    tracing::debug!(
        len = trimmed.len(),
        "[composio-webhook] storing subscription secret (redacted)"
    );
    let auth = AuthService::from_config(config);
    auth.store_provider_token(
        COMPOSIO_WEBHOOK_SECRET_PROVIDER,
        DEFAULT_AUTH_PROFILE_NAME,
        trimmed,
        std::collections::HashMap::new(),
        true,
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Read the per-subscription HMAC signing secret. Returns `Ok(None)`
/// when no secret has been stored — receiver should treat this as
/// "no subscription registered yet" and reject inbound traffic.
pub fn get_composio_webhook_secret(config: &Config) -> Result<Option<String>, String> {
    let auth = AuthService::from_config(config);
    let secret = auth
        .get_provider_bearer_token(COMPOSIO_WEBHOOK_SECRET_PROVIDER, None)
        .map_err(|e| e.to_string())?;
    Ok(secret
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

/// Remove the stored webhook secret. Used by the "reset subscription"
/// flow — paired with a `delete_webhook_subscription_v3` call upstream.
pub fn clear_composio_webhook_secret(config: &Config) -> Result<bool, String> {
    tracing::debug!("[composio-webhook] clearing stored subscription secret");
    let auth = AuthService::from_config(config);
    auth.remove_profile(COMPOSIO_WEBHOOK_SECRET_PROVIDER, DEFAULT_AUTH_PROFILE_NAME)
        .map_err(|e| e.to_string())
}
