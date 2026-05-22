//! Channel controller business logic.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::openhuman::config::{Config, DiscordConfig, IMessageConfig, TelegramConfig};
use crate::openhuman::credentials;
use crate::rpc::RpcOutcome;

use super::definitions::{
    all_channel_definitions, find_channel_definition, ChannelAuthMode, ChannelDefinition,
};

/// Result returned by `connect_channel`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConnectionResult {
    /// `"connected"` for credential-based modes, `"pending_auth"` for OAuth/managed.
    pub status: String,
    /// Whether the service must be restarted for the channel to become active.
    pub restart_required: bool,
    /// For OAuth/managed modes: the action ID the frontend should handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_action: Option<String>,
    /// Human-readable status message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Single entry returned by `channel_status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStatusEntry {
    pub channel_id: String,
    pub auth_mode: ChannelAuthMode,
    pub connected: bool,
    pub has_credentials: bool,
}

/// Result returned by `test_channel`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelTestResult {
    pub success: bool,
    pub message: String,
}

/// Credential provider key for channel connections: `"channel:{id}:{mode}"`.
fn credential_provider(channel_id: &str, mode: ChannelAuthMode) -> String {
    format!("channel:{}:{}", channel_id, mode)
}

fn parse_allowed_users(value: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    let mut push_identity = |raw: &str| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let normalized = trimmed.trim_start_matches('@').trim();
        if normalized.is_empty() {
            return;
        }
        let canonical = normalized.to_lowercase();
        if !out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&canonical))
        {
            out.push(canonical);
        }
    };

    match value {
        Some(Value::String(s)) => {
            for part in s.split([',', '\n', '\r']) {
                push_identity(part);
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    for part in s.split([',', '\n', '\r']) {
                        push_identity(part);
                    }
                }
            }
        }
        _ => {}
    }

    out
}

fn parse_optional_bool(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Number(n)) => n.as_i64().map(|v| v != 0),
        Some(Value::String(s)) => {
            let normalized = s.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

/// List all available channel definitions.
pub async fn list_channels() -> Result<RpcOutcome<Vec<ChannelDefinition>>, String> {
    Ok(RpcOutcome::new(all_channel_definitions(), vec![]))
}

/// Describe a single channel by id.
pub async fn describe_channel(channel_id: &str) -> Result<RpcOutcome<ChannelDefinition>, String> {
    let def = find_channel_definition(channel_id)
        .ok_or_else(|| format!("unknown channel: {channel_id}"))?;
    Ok(RpcOutcome::new(def, vec![]))
}

/// Initiate a channel connection.
///
/// For `BotToken`/`ApiKey` modes: validates fields and stores credentials.
/// For `OAuth`/`ManagedDm` modes: returns the auth action the frontend should handle.
pub async fn connect_channel(
    config: &Config,
    channel_id: &str,
    auth_mode: ChannelAuthMode,
    credentials_value: Value,
) -> Result<RpcOutcome<ChannelConnectionResult>, String> {
    let def = find_channel_definition(channel_id)
        .ok_or_else(|| format!("unknown channel: {channel_id}"))?;

    let spec = def.auth_mode_spec(auth_mode).ok_or_else(|| {
        format!(
            "channel '{}' does not support auth mode '{}'",
            channel_id, auth_mode
        )
    })?;

    // For OAuth/managed modes, return the auth action without storing credentials.
    if let Some(action) = spec.auth_action {
        return Ok(RpcOutcome::new(
            ChannelConnectionResult {
                status: "pending_auth".to_string(),
                restart_required: false,
                auth_action: Some(action.to_string()),
                message: Some(format!("Initiate '{}' auth flow on the frontend. Ignore if you are already in the auth flow.", action)),
            },
            vec![],
        ));
    }

    // Credential-based modes: validate required fields.
    let creds_map = credentials_value
        .as_object()
        .ok_or("credentials must be a JSON object")?;

    def.validate_credentials(auth_mode, creds_map)?;

    // iMessage is local-only (no credentials): persist channels_config + return connected.
    if channel_id == "imessage" && auth_mode == ChannelAuthMode::ManagedDm {
        let allowed_contacts = parse_allowed_users(creds_map.get("allowed_contacts"));
        let allowed_contacts_count = allowed_contacts.len();

        let mut persisted = config.clone();
        persisted.channels_config.imessage = Some(IMessageConfig { allowed_contacts });

        persisted
            .save()
            .await
            .map_err(|e| format!("failed to persist imessage config.toml: {e}"))?;

        tracing::info!(
            target: "openhuman::channels",
            allowed_contacts_count,
            "[imessage] connect_channel: wrote channels_config.imessage; restart core for AppleScript bridge to load"
        );

        return Ok(RpcOutcome::single_log(
            ChannelConnectionResult {
                status: "connected".to_string(),
                restart_required: true,
                auth_action: None,
                message: Some(
                    "iMessage channel configured. Grant Full Disk Access and restart the service to activate.".to_string(),
                ),
            },
            "stored imessage channel config (local-only)".to_string(),
        ));
    }

    // Store credentials via the credentials domain.
    let provider_key = credential_provider(channel_id, auth_mode);

    // Extract the primary token field (bot_token or api_key) if present.
    let token = creds_map
        .get("bot_token")
        .or_else(|| creds_map.get("api_key"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Store remaining fields as metadata.
    let fields = if creds_map.len() > 1 || (creds_map.len() == 1 && token.is_none()) {
        Some(Value::Object(creds_map.clone()))
    } else {
        None
    };

    credentials::ops::store_provider_credentials(
        config,
        &provider_key,
        None, // default profile
        token,
        fields,
        Some(true),
    )
    .await
    .map_err(|e| format!("failed to store credentials: {e}"))?;

    // Keep runtime channel config in sync so listeners can actually start
    // with the credentials just connected from the UI.
    if channel_id == "telegram" && auth_mode == ChannelAuthMode::BotToken {
        let bot_token = creds_map
            .get("bot_token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "missing required bot_token".to_string())?
            .to_string();
        let allowed_users = parse_allowed_users(creds_map.get("allowed_users"));
        let allowed_users_count = allowed_users.len();

        let mut persisted = config.clone();
        let (stream_mode, draft_update_interval_ms, silent_streaming, mention_only) =
            if let Some(existing) = persisted.channels_config.telegram.as_ref() {
                (
                    existing.stream_mode,
                    existing.draft_update_interval_ms,
                    existing.silent_streaming,
                    existing.mention_only,
                )
            } else {
                (
                    crate::openhuman::config::StreamMode::default(),
                    1000,
                    true,
                    false,
                )
            };

        persisted.channels_config.telegram = Some(TelegramConfig {
            bot_token,
            allowed_users,
            stream_mode,
            draft_update_interval_ms,
            silent_streaming,
            mention_only,
        });

        persisted
            .save()
            .await
            .map_err(|e| format!("failed to persist telegram config.toml: {e}"))?;

        let spawned =
            crate::openhuman::channels::runtime::listener_registry::rebuild("telegram", &persisted);
        tracing::info!(
            target: "openhuman::channels",
            allowed_users_count,
            mention_only,
            hot_reloaded = spawned,
            "[telegram] connect_channel: wrote channels_config.telegram and rebuilt listener"
        );
    } else if channel_id == "discord" && auth_mode == ChannelAuthMode::BotToken {
        let bot_token = creds_map
            .get("bot_token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "missing required bot_token".to_string())?
            .to_string();

        let guild_id = creds_map
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let discord_channel_id = creds_map
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let mut persisted = config.clone();
        let existing = persisted.channels_config.discord.as_ref();
        let parsed_allowed_users = parse_allowed_users(creds_map.get("allowed_users"));
        let allowed_users = if parsed_allowed_users.is_empty() {
            existing
                .map(|cfg| cfg.allowed_users.clone())
                .unwrap_or_default()
        } else {
            parsed_allowed_users
        };
        let allowed_users_count = allowed_users.len();
        let listen_to_bots = parse_optional_bool(creds_map.get("listen_to_bots"))
            .unwrap_or_else(|| existing.map(|cfg| cfg.listen_to_bots).unwrap_or(false));
        let mention_only = parse_optional_bool(creds_map.get("mention_only"))
            .unwrap_or_else(|| existing.map(|cfg| cfg.mention_only).unwrap_or(false));

        persisted.channels_config.discord = Some(DiscordConfig {
            bot_token,
            guild_id: guild_id.clone(),
            channel_id: discord_channel_id.clone(),
            allowed_users,
            listen_to_bots,
            mention_only,
        });

        persisted
            .save()
            .await
            .map_err(|e| format!("failed to persist discord config.toml: {e}"))?;

        let spawned =
            crate::openhuman::channels::runtime::listener_registry::rebuild("discord", &persisted);
        tracing::info!(
            target: "openhuman::channels",
            has_guild_id = guild_id.is_some(),
            has_channel_id = discord_channel_id.is_some(),
            allowed_users_count,
            listen_to_bots,
            mention_only,
            hot_reloaded = spawned,
            "[discord] connect_channel: wrote channels_config.discord and rebuilt listener"
        );
    }

    Ok(RpcOutcome::single_log(
        ChannelConnectionResult {
            status: "connected".to_string(),
            restart_required: true,
            auth_action: None,
            message: Some(format!(
                "Channel '{}' credentials stored. Restart the service to activate.",
                channel_id
            )),
        },
        format!("stored credentials for {}", provider_key),
    ))
}

/// Disconnect a channel by removing stored credentials.
pub async fn disconnect_channel(
    config: &Config,
    channel_id: &str,
    auth_mode: ChannelAuthMode,
) -> Result<RpcOutcome<Value>, String> {
    // Verify channel exists.
    find_channel_definition(channel_id).ok_or_else(|| format!("unknown channel: {channel_id}"))?;

    let provider_key = credential_provider(channel_id, auth_mode);

    // iMessage has no stored credentials (local-only); skip credential removal.
    if !(channel_id == "imessage" && auth_mode == ChannelAuthMode::ManagedDm) {
        credentials::ops::remove_provider_credentials(config, &provider_key, None)
            .await
            .map_err(|e| format!("failed to remove credentials: {e}"))?;
    }

    let mut listener_id: Option<&'static str> = None;
    if channel_id == "telegram" && auth_mode == ChannelAuthMode::BotToken {
        let mut persisted = config.clone();
        if persisted.channels_config.telegram.take().is_some() {
            persisted
                .save()
                .await
                .map_err(|e| format!("failed to clear telegram config.toml: {e}"))?;
            tracing::info!(
                target: "openhuman::channels",
                "[telegram] disconnect_channel: cleared channels_config.telegram"
            );
        }
        listener_id = Some("telegram");
    } else if channel_id == "discord" && auth_mode == ChannelAuthMode::BotToken {
        let mut persisted = config.clone();
        if persisted.channels_config.discord.take().is_some() {
            persisted
                .save()
                .await
                .map_err(|e| format!("failed to clear discord config.toml: {e}"))?;
            tracing::info!(
                target: "openhuman::channels",
                "[discord] disconnect_channel: cleared channels_config.discord"
            );
        }
        listener_id = Some("discord");
    } else if channel_id == "imessage" && auth_mode == ChannelAuthMode::ManagedDm {
        let mut persisted = config.clone();
        if persisted.channels_config.imessage.take().is_some() {
            persisted
                .save()
                .await
                .map_err(|e| format!("failed to clear imessage config.toml: {e}"))?;
            tracing::info!(
                target: "openhuman::channels",
                "[imessage] disconnect_channel: cleared channels_config.imessage"
            );
        }
        listener_id = Some("imessage");
    }

    // Tear down any live listener task so the bot stops polling
    // immediately. Without this the existing supervisor would keep
    // calling `getUpdates` with the now-removed token, blocking the
    // next reconnect with a 409 conflict.
    let aborted = if let Some(id) = listener_id {
        crate::openhuman::channels::runtime::listener_registry::abort(id)
    } else {
        false
    };

    Ok(RpcOutcome::single_log(
        json!({
            "channel": channel_id,
            "auth_mode": auth_mode,
            "disconnected": true,
            "listener_aborted": aborted,
            // Hot-reload covers inbound polling; outbound replies
            // through subscribers that captured the old Arc at startup
            // still need a process restart until channels_by_name
            // becomes hot-swappable. Leave `restart_required` true so
            // the UI's existing prompt continues to nudge users until
            // that follow-up lands.
            "restart_required": true,
        }),
        format!("removed credentials for {}", provider_key),
    ))
}

/// Get connection status for one or all channels.
pub async fn channel_status(
    config: &Config,
    channel_id: Option<&str>,
) -> Result<RpcOutcome<Vec<ChannelStatusEntry>>, String> {
    // List all stored credentials with "channel:" prefix. Uses the
    // prefix-match helper because channel credentials are keyed as
    // `channel:<id>:<mode>` and no single literal value matches them
    // through `list_provider_credentials`'s exact-match filter.
    let stored = credentials::ops::list_provider_credentials_by_prefix(config, "channel:")
        .await
        .map_err(|e| format!("failed to list credentials: {e}"))?;

    let stored_providers: Vec<String> = stored.iter().map(|p| p.provider.clone()).collect();

    let defs = match channel_id {
        Some(id) => {
            let def =
                find_channel_definition(id).ok_or_else(|| format!("unknown channel: {id}"))?;
            vec![def]
        }
        None => all_channel_definitions(),
    };

    let mut entries = Vec::new();
    for def in &defs {
        for spec in &def.auth_modes {
            let provider_key = credential_provider(def.id, spec.mode);
            let has_creds = stored_providers.iter().any(|p| p == &provider_key);
            entries.push(ChannelStatusEntry {
                channel_id: def.id.to_string(),
                auth_mode: spec.mode,
                connected: has_creds,
                has_credentials: has_creds,
            });
        }
    }

    Ok(RpcOutcome::new(entries, vec![]))
}

/// Return the slugs of all messaging channels currently connected,
/// merging the two storage layers OpenHuman uses for connection state.
///
/// Two equally-authoritative sources exist today:
///
/// * `config.channels_config.<slug>` — the legacy TOML field set by
///   credential-mode connects that need a runtime listener
///   (`bot_token` / `webhook` / `oauth`). These trigger
///   `restart_required = true` on the connect call.
/// * Provider credentials keyed `channel:<slug>:<mode>` — set by the
///   newer managed-DM and OAuth flows that don't materialise a TOML
///   block but do persist a credential marker.
///
/// Until both stores merge, any caller that only reads one will report
/// stale state to the user (e.g. the agent will say "Telegram not
/// connected" right after a managed-DM link succeeds — issue #1149).
/// This helper centralises the merge so every consumer agrees.
pub async fn connected_channel_slugs(config: &Config) -> Result<Vec<String>, String> {
    use std::collections::BTreeSet;

    let mut slugs: BTreeSet<String> = BTreeSet::new();

    // Layer 1: credential-mode channels written to TOML config.
    let cc = &config.channels_config;
    if cc.telegram.is_some() {
        slugs.insert("telegram".to_string());
    }
    if cc.discord.is_some() {
        slugs.insert("discord".to_string());
    }
    if cc.slack.is_some() {
        slugs.insert("slack".to_string());
    }
    if cc.mattermost.is_some() {
        slugs.insert("mattermost".to_string());
    }
    if cc.email.is_some() {
        slugs.insert("email".to_string());
    }
    if cc.whatsapp.is_some() {
        slugs.insert("whatsapp".to_string());
    }
    if cc.signal.is_some() {
        slugs.insert("signal".to_string());
    }
    if cc.matrix.is_some() {
        slugs.insert("matrix".to_string());
    }
    if cc.imessage.is_some() {
        slugs.insert("imessage".to_string());
    }
    if cc.irc.is_some() {
        slugs.insert("irc".to_string());
    }
    if cc.lark.is_some() {
        slugs.insert("lark".to_string());
    }
    if cc.dingtalk.is_some() {
        slugs.insert("dingtalk".to_string());
    }
    if cc.linq.is_some() {
        slugs.insert("linq".to_string());
    }
    if cc.qq.is_some() {
        slugs.insert("qq".to_string());
    }

    // Layer 2: managed-DM / OAuth channels stored only as credentials
    // under `channel:<slug>:<mode>`.
    let stored = credentials::ops::list_provider_credentials_by_prefix(config, "channel:")
        .await
        .map_err(|e| format!("failed to list channel credentials: {e}"))?;
    for entry in &stored {
        // provider format: "channel:<slug>:<mode>" — extract slug.
        if let Some(rest) = entry.provider.strip_prefix("channel:") {
            if let Some((slug, _mode)) = rest.split_once(':') {
                if !slug.is_empty() {
                    slugs.insert(slug.to_string());
                }
            }
        }
    }

    Ok(slugs.into_iter().collect())
}

/// Test a channel connection without persisting credentials.
pub async fn test_channel(
    _config: &Config,
    channel_id: &str,
    auth_mode: ChannelAuthMode,
    credentials_value: Value,
) -> Result<RpcOutcome<ChannelTestResult>, String> {
    let def = find_channel_definition(channel_id)
        .ok_or_else(|| format!("unknown channel: {channel_id}"))?;

    let creds_map = credentials_value
        .as_object()
        .ok_or("credentials must be a JSON object")?;

    // Validate fields first.
    def.validate_credentials(auth_mode, creds_map)?;

    // For now, field validation is the test. A future version can instantiate
    // the channel provider and call health_check().
    Ok(RpcOutcome::new(
        ChannelTestResult {
            success: true,
            message: format!(
                "Credentials for '{}' ({}) are structurally valid.",
                channel_id, auth_mode
            ),
        },
        vec![],
    ))
}

// ---------------------------------------------------------------------------
// Managed Telegram login flow
// ---------------------------------------------------------------------------

fn backend_managed_channels_removed() -> String {
    "backend-managed channel flows were removed; connect channels with native bot tokens, API keys, or provider OAuth".to_string()
}

/// Result from `telegram_login_start`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramLoginStartResult {
    /// The short-lived link token created by the backend.
    pub link_token: String,
    /// Full Telegram deep link URL the user should open.
    pub telegram_url: String,
    /// Bot username used.
    pub bot_username: String,
}

/// Result from `telegram_login_check`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramLoginCheckResult {
    /// Whether the Telegram user has been linked to the app user.
    pub linked: bool,
    /// Backend-provided status payload (may include telegramUserId, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Step 1: Create a channel link token for Telegram and return the deep link URL.
///
/// Removed with the hosted managed-channel backend.
pub async fn telegram_login_start(
    _config: &Config,
) -> Result<RpcOutcome<TelegramLoginStartResult>, String> {
    Err(backend_managed_channels_removed())
}

/// Step 2: Check whether the user has completed the Telegram link (clicked /start).
///
/// Removed with the hosted managed-channel backend.
pub async fn telegram_login_check(
    _config: &Config,
    _link_token: &str,
) -> Result<RpcOutcome<TelegramLoginCheckResult>, String> {
    Err(backend_managed_channels_removed())
}

// ---------------------------------------------------------------------------
// Discord managed link flow
// ---------------------------------------------------------------------------

/// Result from `discord_link_start`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscordLinkStartResult {
    /// The short-lived link token to paste into Discord.
    pub link_token: String,
    /// Human-readable instruction shown to the user.
    pub instructions: String,
}

/// Result from `discord_link_check`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscordLinkCheckResult {
    /// Whether the Discord account has been linked to the app user.
    pub linked: bool,
    /// Removed backend status payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Step 1: Create a Discord channel link token.
///
/// Returns a short-lived token the user pastes into Discord as `!start <token>`.
/// Removed with the hosted managed-channel backend.
pub async fn discord_link_start(
    _config: &Config,
) -> Result<RpcOutcome<DiscordLinkStartResult>, String> {
    Err(backend_managed_channels_removed())
}

/// Step 2: Check whether the user has completed the Discord link.
///
/// Removed with the hosted managed-channel backend.
pub async fn discord_link_check(
    _config: &Config,
    _link_token: &str,
) -> Result<RpcOutcome<DiscordLinkCheckResult>, String> {
    Err(backend_managed_channels_removed())
}

// ---------------------------------------------------------------------------
// Channel messaging, reactions, and thread management
// ---------------------------------------------------------------------------

/// Send a rich message to a channel via the backend API.
pub async fn channel_send_message(
    _config: &Config,
    _channel: &str,
    _message: Value,
) -> Result<RpcOutcome<Value>, String> {
    Err(backend_managed_channels_removed())
}

/// Send a reaction to a message in a channel via the backend API.
pub async fn channel_send_reaction(
    _config: &Config,
    _channel: &str,
    _reaction: Value,
) -> Result<RpcOutcome<Value>, String> {
    Err(backend_managed_channels_removed())
}

/// Create a thread in a channel via the backend API.
pub async fn channel_create_thread(
    _config: &Config,
    _channel: &str,
    _title: &str,
) -> Result<RpcOutcome<Value>, String> {
    Err(backend_managed_channels_removed())
}

/// Close or reopen a thread in a channel via the backend API.
pub async fn channel_update_thread(
    _config: &Config,
    _channel: &str,
    _thread_id: &str,
    _action: &str,
) -> Result<RpcOutcome<Value>, String> {
    Err(backend_managed_channels_removed())
}

/// List threads in a channel via the backend API.
pub async fn channel_list_threads(
    _config: &Config,
    _channel: &str,
    _active: Option<bool>,
) -> Result<RpcOutcome<Value>, String> {
    Err(backend_managed_channels_removed())
}

// ---------------------------------------------------------------------------
// Discord guild/channel discovery
// ---------------------------------------------------------------------------

/// Retrieve the stored Discord bot token from credentials.
async fn discord_bot_token(config: &Config) -> Result<String, String> {
    let provider_key = credential_provider("discord", ChannelAuthMode::BotToken);
    let auth = credentials::AuthService::from_config(config);
    let profile = auth
        .get_profile(&provider_key, None)
        .map_err(|e| format!("failed to load Discord credentials: {e}"))?
        .ok_or("Discord bot token not configured. Connect Discord first.")?;

    let token = profile.token.unwrap_or_default();
    if token.is_empty() {
        return Err("Discord bot token is empty.".to_string());
    }
    Ok(token)
}

/// List Discord guilds (servers) the connected bot is a member of.
pub async fn discord_list_guilds(
    config: &Config,
) -> Result<
    RpcOutcome<Vec<crate::openhuman::channels::providers::discord::api::DiscordGuild>>,
    String,
> {
    use crate::openhuman::channels::providers::discord::api;

    let token = discord_bot_token(config).await?;
    let guilds = api::list_bot_guilds(&token)
        .await
        .map_err(|e| format!("Discord API error: {e}"))?;
    Ok(RpcOutcome::single_log(guilds, "discord guilds listed"))
}

/// List text channels in a Discord guild.
pub async fn discord_list_channels(
    config: &Config,
    guild_id: &str,
) -> Result<
    RpcOutcome<Vec<crate::openhuman::channels::providers::discord::api::DiscordTextChannel>>,
    String,
> {
    use crate::openhuman::channels::providers::discord::api;

    if guild_id.is_empty() {
        return Err("guild_id is required".to_string());
    }
    let token = discord_bot_token(config).await?;
    let channels = api::list_guild_channels(&token, guild_id)
        .await
        .map_err(|e| format!("Discord API error: {e}"))?;
    Ok(RpcOutcome::single_log(
        channels,
        format!("discord channels listed for guild {guild_id}"),
    ))
}

/// Check bot permissions in a Discord channel.
pub async fn discord_check_permissions(
    config: &Config,
    guild_id: &str,
    channel_id: &str,
) -> Result<
    RpcOutcome<crate::openhuman::channels::providers::discord::api::BotPermissionCheck>,
    String,
> {
    use crate::openhuman::channels::providers::discord::api;

    if guild_id.is_empty() || channel_id.is_empty() {
        return Err("guild_id and channel_id are required".to_string());
    }
    let token = discord_bot_token(config).await?;
    let check = api::check_channel_permissions(&token, guild_id, channel_id)
        .await
        .map_err(|e| format!("Discord API error: {e}"))?;
    Ok(RpcOutcome::single_log(
        check,
        format!("discord permissions checked for channel {channel_id}"),
    ))
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
