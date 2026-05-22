//! Direct Composio v3 client factory and response shapers.
//!
//! The hosted OpenHuman backend proxy has been removed from this fork.
//! Composio calls now use the user's own Composio API key against the
//! public Composio v3 API via [`crate::openhuman::tools::ComposioTool`].

use super::types::{
    ComposioActiveTrigger, ComposioActiveTriggersResponse, ComposioAuthorizeResponse,
    ComposioAvailableTrigger, ComposioAvailableTriggersResponse, ComposioConnectionsResponse,
    ComposioCreateTriggerResponse, ComposioDeleteResponse, ComposioDisableTriggerResponse,
    ComposioEnableTriggerResponse, ComposioExecuteResponse, ComposioToolsResponse,
};
use std::sync::Arc;

// ── Direct-mode factory ─────────────────────────────────────────────
//
// Mirrors `src/openhuman/embeddings/factory.rs` so anyone reading both
// can pattern-match between domains: string-matched mode, explicit error
// on unknown mode, explicit error when `direct` is selected without an
// API key.

use crate::openhuman::config::schema::COMPOSIO_MODE_DIRECT;

const MODE_DIRECT_PAT: &str = COMPOSIO_MODE_DIRECT;

/// Tagged variant returned by [`create_composio_client`].
pub enum ComposioClientKind {
    Direct(Arc<crate::openhuman::tools::ComposioTool>),
}

impl ComposioClientKind {
    /// Returns `"direct"` — handy for logging and tests.
    pub fn mode(&self) -> &'static str {
        match self {
            ComposioClientKind::Direct(_) => COMPOSIO_MODE_DIRECT,
        }
    }
}

/// Construct the direct Composio client from the root config.
pub fn create_composio_client(
    config: &crate::openhuman::config::Config,
) -> anyhow::Result<ComposioClientKind> {
    let raw_mode = config.composio.mode.trim();
    let mode = if raw_mode.is_empty() || raw_mode.eq_ignore_ascii_case("backend") {
        COMPOSIO_MODE_DIRECT
    } else {
        raw_mode
    };
    tracing::debug!(mode = %mode, "[composio-factory] resolving client");

    match mode {
        MODE_DIRECT_PAT => {
            // Prefer keychain-stored key; fall back to `config.toml`.
            let stored = crate::openhuman::credentials::get_composio_api_key(config)
                .map_err(|e| anyhow::anyhow!("failed to read stored composio api key: {e}"))?;
            let api_key = stored
                .or_else(|| {
                    config
                        .composio
                        .api_key
                        .as_ref()
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty())
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "composio direct mode selected but no api key is configured \
                         (set via composio.set_api_key RPC or config.composio.api_key)"
                    )
                })?;

            // The direct client takes a `SecurityPolicy` for `Tool::execute`
            // gating, but the factory's job is only to materialize a *client*
            // — it does not actually invoke `execute()` itself, so the
            // default policy is sufficient here. Callers that go through
            // the `Tool` surface re-acquire the live policy from their own
            // context.
            let security = Arc::new(crate::openhuman::security::SecurityPolicy::default());
            let tool = crate::openhuman::tools::ComposioTool::new(
                &api_key,
                Some(config.composio.entity_id.as_str()),
                security,
            );
            tracing::debug!(
                key_len = api_key.len(),
                "[composio-factory] resolved direct variant (key redacted)"
            );
            Ok(ComposioClientKind::Direct(Arc::new(tool)))
        }
        unknown => {
            tracing::warn!(mode = %unknown, "[composio-factory] unknown composio mode");
            Err(anyhow::anyhow!(
                "unknown composio mode: \"{unknown}\". Supported: \"direct\""
            ))
        }
    }
}

// ── Direct-mode response reshapers ──────────────────────────────────
//
// The direct-mode `ComposioTool` (in `tools/impl/network/composio.rs`)
// speaks `backend.composio.dev/api/v3/*` natively. The helpers below
// reshape those v3 responses into the envelopes consumed by the existing
// RPC, event-bus, and frontend contracts.
//
// All three helpers live next to the factory so anyone touching the
// direct-mode plumbing can see the full envelope-translation surface
// in one place.

use super::types::ComposioConnection;

/// Calls Composio v3 `/connected_accounts/link` via
/// [`crate::openhuman::tools::ComposioTool::get_connection_url`] and
/// reshapes the response into the [`ComposioAuthorizeResponse`] the UI
/// already consumes.
///
/// The v3 endpoint returns a redirect URL but does NOT (currently)
/// surface a stable `connection_id` in the same call — the connection
/// row is created lazily when the user completes OAuth on Composio's
/// hosted page. To preserve the response contract the frontend already
/// consumes, we emit an empty `connection_id` for now. The 5 s
/// `list_connections` poll (now live in direct mode too — see
/// [`direct_list_connections`]) is what ultimately surfaces the new
/// row to the UI.
pub(super) async fn direct_authorize(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    toolkit: &str,
    entity_id: &str,
) -> anyhow::Result<ComposioAuthorizeResponse> {
    let toolkit = toolkit.trim();
    if toolkit.is_empty() {
        anyhow::bail!("composio direct authorize: toolkit must not be empty");
    }
    let entity_id = entity_id.trim();
    let entity_id = if entity_id.is_empty() {
        "default"
    } else {
        entity_id
    };
    tracing::debug!(
        toolkit = %toolkit,
        entity_id = %entity_id,
        "[composio-direct] authorize: requesting hosted connect URL"
    );
    let connect_url = direct
        .get_connection_url(Some(toolkit), None, entity_id)
        .await?;
    tracing::debug!(
        toolkit = %toolkit,
        url_len = connect_url.len(),
        "[composio-direct] authorize: got connect url (redacted)"
    );
    Ok(ComposioAuthorizeResponse {
        connect_url,
        // No stable connection id in the v3 link response — see fn-level
        // doc. The frontend uses `connectUrl` to open the browser and
        // `listConnections` polling to detect the resulting row.
        connection_id: String::new(),
    })
}

/// Direct-mode counterpart to [`ComposioClient::execute_tool`]. Mirrors
/// the v3 `/tools/{slug}/execute` envelope into [`ComposioExecuteResponse`]
/// so the caller doesn't branch on mode for the
/// `ComposioActionExecuted` event-bus payload or the
/// markdown-vs-JSON-body preference.
///
/// Direct mode runs without the backend's billing margin, so `cost_usd`
/// is reported as `0.0`. The backend's `markdownFormatted` field is
/// likewise specific to the backend-proxied path and remains `None` for
/// direct callers, which fall back to the raw JSON envelope.
pub async fn direct_execute(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    tool: &str,
    arguments: Option<serde_json::Value>,
    entity_id: &str,
) -> anyhow::Result<ComposioExecuteResponse> {
    let tool = tool.trim();
    if tool.is_empty() {
        anyhow::bail!("composio direct_execute: tool slug must not be empty");
    }
    let params = arguments.unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    let entity_id = entity_id.trim();
    let entity_id_opt = (!entity_id.is_empty()).then_some(entity_id);
    tracing::debug!(
        tool = %tool,
        has_entity = entity_id_opt.is_some(),
        "[composio-direct] execute: invoking v3 /tools/{{slug}}/execute"
    );
    let raw = direct
        .execute_action(tool, params, entity_id_opt, None)
        .await?;
    // v3 surfaces `successful` + `data` + `error` at the top level. If
    // none are present, treat the call as success so callers see the
    // raw payload instead of an empty error envelope.
    let successful = raw
        .get("successful")
        .and_then(serde_json::Value::as_bool)
        .or_else(|| raw.get("success").and_then(serde_json::Value::as_bool))
        .unwrap_or(true);
    let error = raw
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let data = raw.get("data").cloned().unwrap_or(raw);
    Ok(ComposioExecuteResponse {
        data,
        successful,
        error,
        cost_usd: 0.0,
        markdown_formatted: None,
    })
}

/// Direct-mode counterpart to [`ComposioClient::list_connections`].
///
/// Calls Composio v3 `/connected_accounts` (via
/// [`crate::openhuman::tools::ComposioTool::list_connected_accounts`])
/// and maps each item to the canonical [`ComposioConnection`] so the
/// existing frontend type contract and the 5 s UI poll keep working
/// unchanged.
///
/// Toolkit slug, status, and `created_at` are extracted defensively —
/// missing or unparseable fields fall back to empty strings / `None`
/// rather than dropping the row. The status filter applied downstream
/// (`ComposioConnection::is_active`) treats empty status as inactive,
/// so a malformed row will simply not be presented as connected — the
/// fail-safe shape the user expects.
/// Direct-mode counterpart to [`ComposioClient::delete_connection`].
/// Calls Composio v3 `DELETE /connected_accounts/{id}` against the
/// user's personal tenant (via
/// [`crate::openhuman::tools::ComposioTool::delete_connected_account`])
/// and returns the same [`ComposioDeleteResponse { deleted: true }`]
/// shape the backend-proxied path emits so the
/// [`composio_delete_connection`] op call site stays single-shape.
///
/// [`composio_delete_connection`]:
///     crate::openhuman::composio::ops::composio_delete_connection
pub async fn direct_delete_connection(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    connection_id: &str,
) -> anyhow::Result<ComposioDeleteResponse> {
    tracing::debug!(
        connection_id,
        "[composio-direct] delete_connection: DELETE v3 /connected_accounts/{{id}}"
    );
    direct.delete_connected_account(connection_id).await?;
    Ok(ComposioDeleteResponse { deleted: true })
}

pub async fn direct_list_connections(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
) -> anyhow::Result<ComposioConnectionsResponse> {
    tracing::debug!("[composio-direct] list_connections: GET v3 /connected_accounts");
    let items = direct.list_connected_accounts().await?;
    let connections: Vec<ComposioConnection> = items
        .into_iter()
        .filter_map(|item| {
            let id = item.id.trim().to_string();
            if id.is_empty() {
                return None;
            }
            let toolkit = item.toolkit_slug().unwrap_or_default();
            let status = item.status.clone().unwrap_or_default();
            Some(ComposioConnection {
                id,
                toolkit,
                status,
                created_at: item.created_at.clone(),
            })
        })
        .collect();
    tracing::debug!(
        count = connections.len(),
        "[composio-direct] list_connections: mapped v3 connected accounts"
    );
    Ok(ComposioConnectionsResponse { connections })
}

/// Derive a toolkit slug from a Composio trigger slug.
///
/// Composio v3 trigger slugs prefix the toolkit (e.g.
/// `GMAIL_NEW_GMAIL_MESSAGE` → `gmail`, `SLACK_RECEIVE_MESSAGE` →
/// `slack`). The `trigger_instances/active` endpoint does not echo back
/// the toolkit explicitly, so we derive it from the prefix to populate
/// [`ComposioActiveTrigger::toolkit`].
///
/// Returns an empty string when the slug has no underscore — callers
/// treat that as "unknown toolkit" rather than panic.
fn derive_toolkit_from_trigger_slug(slug: &str) -> String {
    slug.split_once('_')
        .map(|(prefix, _)| prefix.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Direct-mode counterpart to [`ComposioClient::list_active_triggers`].
///
/// Calls Composio v3 `GET /trigger_instances/active` (via
/// [`crate::openhuman::tools::ComposioTool::list_active_triggers_v3`])
/// and reshapes each row into the canonical [`ComposioActiveTrigger`]
/// envelope. `toolkit` filtering is done client-side since the v3
/// endpoint does not expose a single-toolkit query parameter on the
/// row (only on `connected_account_id` / `trigger_names` lists).
///
/// State derivation: v3's `disabled_at` (ISO timestamp, nullable)
/// indicates whether a row is currently disabled. We map non-null →
/// `"DISABLED"` and null → `"ENABLED"` so the existing UI badge keeps
/// rendering the right state.
pub async fn direct_list_active_triggers(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    toolkit_filter: Option<&str>,
) -> anyhow::Result<ComposioActiveTriggersResponse> {
    tracing::debug!(
        toolkit_filter,
        "[composio-direct] list_active_triggers: GET v3 /trigger_instances/active"
    );
    let raw_items = direct.list_active_triggers_v3().await?;
    let normalized_filter = toolkit_filter
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase());

    let mut triggers: Vec<ComposioActiveTrigger> = Vec::with_capacity(raw_items.len());
    for item in raw_items {
        let id = item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| item.get("uuid").and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let slug = item
            .get("trigger_name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| item.get("triggerName").and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_string();
        let toolkit = derive_toolkit_from_trigger_slug(&slug);
        if let Some(filter) = &normalized_filter {
            if &toolkit != filter {
                continue;
            }
        }
        let connection_id = item
            .get("connected_account_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                item.get("connectedAccountId")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or_default()
            .trim()
            .to_string();
        let trigger_config = item
            .get("trigger_config")
            .or_else(|| item.get("triggerConfig"))
            .cloned();
        let disabled = item
            .get("disabled_at")
            .or_else(|| item.get("disabledAt"))
            .map(|value| !value.is_null())
            .unwrap_or(false);
        let state = Some(if disabled { "DISABLED" } else { "ENABLED" }.to_string());

        triggers.push(ComposioActiveTrigger {
            id,
            slug,
            toolkit,
            connection_id,
            trigger_config,
            state,
        });
    }
    tracing::debug!(
        count = triggers.len(),
        "[composio-direct] list_active_triggers: mapped v3 trigger instances"
    );
    Ok(ComposioActiveTriggersResponse { triggers })
}

/// Direct-mode counterpart to
/// [`ComposioClient::list_available_triggers`].
///
/// Calls Composio v3 `GET /triggers_types?toolkit_slugs=<toolkit>` (via
/// [`crate::openhuman::tools::ComposioTool::list_trigger_types_v3`])
/// and reshapes each row into the canonical
/// [`ComposioAvailableTrigger`] envelope.
///
/// Scope semantics: v3 does not surface the backend's `github_repo`
/// fan-out (the backend used to expand a single static catalog entry
/// into one entry per accessible repo on the user's connection). All
/// entries surface as `scope="static"` so callers that branched on
/// scope still compile — direct mode users with a GitHub connection
/// will see the single catalog entry rather than per-repo rows.
///
/// `default_config` is populated from Composio v3's `config` field
/// (the JSON-Schema-like descriptor of required setup parameters) so
/// the existing config-key extraction in `required_config_keys` keeps
/// working unchanged.
pub async fn direct_list_available_triggers(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    toolkit: &str,
) -> anyhow::Result<ComposioAvailableTriggersResponse> {
    let toolkit_slug = toolkit.trim().to_ascii_lowercase();
    if toolkit_slug.is_empty() {
        anyhow::bail!("composio direct_list_available_triggers: toolkit must not be empty");
    }
    tracing::debug!(
        toolkit = %toolkit_slug,
        "[composio-direct] list_available_triggers: GET v3 /triggers_types"
    );
    let raw_items = direct.list_trigger_types_v3(&toolkit_slug).await?;
    let triggers: Vec<ComposioAvailableTrigger> = raw_items
        .into_iter()
        .filter_map(|item| {
            let slug = item
                .get("slug")
                .and_then(serde_json::Value::as_str)?
                .trim()
                .to_string();
            if slug.is_empty() {
                return None;
            }
            let raw_config = item.get("config").cloned();
            tracing::debug!(
                trigger_slug = %slug,
                raw_config = ?raw_config,
                "[composio-direct] parsing trigger config schema"
            );
            // Composio v3 returns trigger `config` as a JSON Schema:
            //   { "type": "object",
            //     "properties": { "owner": {…}, "repo": {…} },
            //     "required": ["owner", "repo"] }
            // For backwards compatibility we also accept a flat-map
            // shape `{ "field": { "required": true } }` (some non-GitHub
            // toolkits historically used this). Whichever shape comes
            // back, surface a flat `required_config_keys: ["owner",
            // "repo"]` so the renderer can show the inline form without
            // branching on schema version.
            let required_config_keys = raw_config.as_ref().and_then(extract_required_keys);
            // `default_config` is the *prefilled values* the UI seeds
            // its form with — not the schema. Pull `default` per
            // property when present; otherwise return an empty map so
            // the renderer's `defaultConfig[key] ?? ""` falls back
            // cleanly. The full schema would break the form input
            // since the renderer treats `defaultConfig[owner]` as a
            // string, not as `{type, title, description}`.
            let default_config = raw_config.as_ref().map(extract_default_values);
            Some(ComposioAvailableTrigger {
                slug,
                scope: "static".to_string(),
                default_config,
                required_config_keys,
                repo: None,
            })
        })
        .collect();
    tracing::debug!(
        count = triggers.len(),
        "[composio-direct] list_available_triggers: mapped v3 trigger types"
    );
    Ok(ComposioAvailableTriggersResponse { triggers })
}

/// Extract the canonical `required_config_keys` list from a v3 trigger
/// `config` value, tolerating two shapes Composio has used historically:
///
/// 1. **JSON Schema** (current — GitHub, Slack, most modern toolkits):
///    `{ "type": "object", "properties": {…}, "required": ["owner", "repo"] }`
///    → read `required` directly as a `Vec<String>`.
/// 2. **Flat per-field map** (legacy fallback):
///    `{ "channel": { "required": true }, "filter": { "required": false } }`
///    → return the names with `required: true`.
///
/// Returns `None` only when the shape matches neither — leaves the
/// renderer's `requiredConfigKeys ?? []` fallback in charge of the rest.
fn extract_required_keys(config: &serde_json::Value) -> Option<Vec<String>> {
    let obj = config.as_object()?;
    if let Some(required_arr) = obj.get("required").and_then(serde_json::Value::as_array) {
        let mut keys: Vec<String> = required_arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        keys.sort();
        return Some(keys);
    }
    let mut keys: Vec<String> = obj
        .iter()
        .filter_map(|(name, descriptor)| {
            let is_required = descriptor
                .get("required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            is_required.then(|| name.clone())
        })
        .collect();
    keys.sort();
    Some(keys)
}

/// Extract a flat `{key: default_value}` map from a v3 trigger `config`
/// value. The renderer treats `defaultConfig` as the *prefilled form
/// values*, not as the schema — passing the raw JSON Schema (with nested
/// `{type, title, description}` objects) would crash the input controls.
///
/// Reads `properties.<key>.default` per JSON Schema; returns an empty map
/// when no defaults are declared (so the form starts blank, which is the
/// right behaviour for required-without-default fields like `owner`/`repo`).
fn extract_default_values(config: &serde_json::Value) -> serde_json::Value {
    let Some(obj) = config.as_object() else {
        return serde_json::json!({});
    };
    let mut out = serde_json::Map::new();
    if let Some(props) = obj.get("properties").and_then(serde_json::Value::as_object) {
        for (key, descriptor) in props {
            if let Some(default) = descriptor.get("default") {
                out.insert(key.clone(), default.clone());
            }
        }
    } else {
        // Legacy flat-map shape — pull `default` straight off the
        // per-field descriptor.
        for (key, descriptor) in obj {
            if let Some(default) = descriptor.get("default") {
                out.insert(key.clone(), default.clone());
            }
        }
    }
    serde_json::Value::Object(out)
}

/// Direct-mode counterpart to
/// [`ComposioClient::enable_trigger`].
///
/// Calls Composio v3
/// `POST /trigger_instances/{slug}/upsert` (via
/// [`crate::openhuman::tools::ComposioTool::upsert_trigger_instance_v3`])
/// and reshapes the response into the canonical
/// [`ComposioEnableTriggerResponse`] envelope so the
/// `composio_enable_trigger` op stays single-shape across both modes.
///
/// Composio's upsert returns either a freshly-created trigger row or
/// the existing one updated in place — either way the response carries
/// a `trigger_id` (v3 nano id) and the `trigger_name` we sent. We
/// derive the canonical `slug` from the request when the response
/// omits it; backend-mode callers see exactly the same field set so
/// downstream consumers (log emitters, frontend state) don't branch on
/// mode.
pub async fn direct_enable_trigger(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    connection_id: &str,
    slug: &str,
    trigger_config: Option<serde_json::Value>,
) -> anyhow::Result<ComposioEnableTriggerResponse> {
    let connection_id = connection_id.trim();
    let slug = slug.trim();
    if connection_id.is_empty() {
        anyhow::bail!("composio direct_enable_trigger: connection_id must not be empty");
    }
    if slug.is_empty() {
        anyhow::bail!("composio direct_enable_trigger: slug must not be empty");
    }
    tracing::debug!(
        connection_id,
        slug,
        "[composio-direct] enable_trigger: POST v3 /trigger_instances/{{slug}}/upsert"
    );
    let raw = direct
        .upsert_trigger_instance_v3(slug, Some(connection_id), trigger_config)
        .await?;

    let trigger_id = raw
        .get("trigger_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| raw.get("triggerId").and_then(serde_json::Value::as_str))
        .or_else(|| raw.get("id").and_then(serde_json::Value::as_str))
        .or_else(|| raw.get("uuid").and_then(serde_json::Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_string();
    if trigger_id.is_empty() {
        anyhow::bail!(
            "composio direct_enable_trigger: Composio response is missing trigger_id ({raw})"
        );
    }

    Ok(ComposioEnableTriggerResponse {
        trigger_id,
        slug: slug.to_string(),
        connection_id: connection_id.to_string(),
    })
}

/// Direct-mode counterpart to
/// [`ComposioClient::disable_trigger`].
///
/// Calls Composio v3
/// `DELETE /trigger_instances/manage/{trigger_id}` (via
/// [`crate::openhuman::tools::ComposioTool::delete_trigger_instance_v3`])
/// and returns `ComposioDisableTriggerResponse { deleted: true }` on
/// success — same single-shape contract the backend path emits so the
/// op layer doesn't branch on mode.
///
/// We use DELETE rather than PATCH(status=disable) here because the
/// `composio_disable_trigger` op semantically means "remove this
/// trigger entirely" (the user has un-toggled it in the UI). Callers
/// who want pause-without-delete can use the PATCH path via
/// [`crate::openhuman::tools::ComposioTool::manage_trigger_instance_v3`]
/// directly — no op layer exposes it today.
pub async fn direct_disable_trigger(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    trigger_id: &str,
) -> anyhow::Result<ComposioDisableTriggerResponse> {
    let trigger_id = trigger_id.trim();
    if trigger_id.is_empty() {
        anyhow::bail!("composio direct_disable_trigger: trigger_id must not be empty");
    }
    tracing::debug!(
        trigger_id,
        "[composio-direct] disable_trigger: DELETE v3 /trigger_instances/manage/{{id}}"
    );
    direct.delete_trigger_instance_v3(trigger_id).await?;
    Ok(ComposioDisableTriggerResponse { deleted: true })
}

/// Direct-mode counterpart to
/// [`ComposioClient::create_trigger`].
///
/// `create_trigger` is semantically equivalent to `enable_trigger`
/// when the trigger does not yet exist — both end up at v3's
/// `upsert` endpoint. The difference is the op-layer shape:
/// `enable_trigger` returns `{ trigger_id, slug, connection_id }`;
/// `create_trigger` returns `{ trigger_id, status }`.
///
/// We reuse the same upstream call (via
/// [`crate::openhuman::tools::ComposioTool::upsert_trigger_instance_v3`])
/// and emit the `create`-shaped envelope. The `status` field is
/// always `"active"` in direct mode because the upsert path always
/// activates — Composio has no notion of a "pending"
/// upsert-but-not-yet-active state for v3 instances.
pub async fn direct_create_trigger(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    slug: &str,
    connection_id: Option<&str>,
    trigger_config: Option<serde_json::Value>,
) -> anyhow::Result<ComposioCreateTriggerResponse> {
    let slug = slug.trim();
    if slug.is_empty() {
        anyhow::bail!("composio direct_create_trigger: slug must not be empty");
    }
    let trimmed_connection = connection_id.map(str::trim).filter(|c| !c.is_empty());
    tracing::debug!(
        slug,
        connection_id = trimmed_connection,
        "[composio-direct] create_trigger: POST v3 /trigger_instances/{{slug}}/upsert"
    );
    let raw = direct
        .upsert_trigger_instance_v3(slug, trimmed_connection, trigger_config)
        .await?;

    let trigger_id = raw
        .get("trigger_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| raw.get("triggerId").and_then(serde_json::Value::as_str))
        .or_else(|| raw.get("id").and_then(serde_json::Value::as_str))
        .or_else(|| raw.get("uuid").and_then(serde_json::Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_string();
    if trigger_id.is_empty() {
        anyhow::bail!(
            "composio direct_create_trigger: Composio response is missing trigger_id ({raw})"
        );
    }

    Ok(ComposioCreateTriggerResponse {
        trigger_id,
        status: Some("active".to_string()),
    })
}

/// Direct-mode counterpart to [`ComposioClient::list_tools`]. Calls
/// Composio v3 `/tools?toolkits=<csv>` via
/// [`crate::openhuman::tools::ComposioTool::list_tool_schemas_v3`] and
/// reshapes each item into the same [`ComposioToolSchema`] envelope the
/// backend-proxied path returns.
///
/// `toolkits` may be empty (full direct-tenant catalogue) or scoped to
/// the user's connected toolkits (preferred — keeps response size bounded
/// and skips schemas the agent can't actually call). `composio_list_tools`'s
/// direct branch passes `direct_list_connections`'s active set.
///
/// Schemas surfaced here are tenant-agnostic — Composio's action
/// definitions are the same across tenants, so direct-mode users get
/// the same model-callable shape backend-mode does. Downstream curated-
/// whitelist filtering (`evaluate_tool_visibility` / `find_curated`)
/// still applies at the `ops::composio_list_tools` layer.
pub(super) async fn direct_list_tools(
    direct: &Arc<crate::openhuman::tools::ComposioTool>,
    toolkits: &[String],
) -> anyhow::Result<ComposioToolsResponse> {
    let toolkit_refs: Vec<&str> = toolkits.iter().map(|s| s.as_str()).collect();
    tracing::debug!(
        toolkits = toolkit_refs.len(),
        "[composio-direct] list_tools: GET v3 /tools"
    );
    let items = direct.list_tool_schemas_v3(&toolkit_refs).await?;
    let tools: Vec<super::types::ComposioToolSchema> = items
        .into_iter()
        .filter(|item| !item.slug.is_empty())
        .map(|item| super::types::ComposioToolSchema {
            kind: "function".to_string(),
            function: super::types::ComposioToolFunction {
                name: item.slug,
                description: item.description,
                parameters: item.input_parameters,
            },
        })
        .collect();
    tracing::debug!(
        count = tools.len(),
        "[composio-direct] list_tools: mapped v3 tool schemas"
    );
    Ok(ComposioToolsResponse { tools })
}
