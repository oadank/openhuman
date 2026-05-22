//! Unified chat-provider factory.
//!
//! Resolves workload names (e.g. `"reasoning"`, `"heartbeat"`) to a
//! `(Box<dyn Provider>, String)` tuple where the second element is the model
//! id to pass into `chat_with_history` / `simple_chat`.
//!
//! ## Provider-string grammar
//!
//! ```text
//! "ollama:<model>"   → local Ollama at config.local_ai.base_url
//! "<slug>:<model>"   → cloud_providers entry keyed by slug;
//!                      builds OpenAiCompatibleProvider (Bearer) or Anthropic
//!                      flavour depending on auth_style.
//! ""  / missing      → chat roles use `default_model`; other roles fall
//!                      back to primary_cloud / first configured provider.
//! ```
//!
//! Unknown slugs and missing-creds configurations produce actionable errors.

use crate::openhuman::config::schema::cloud_providers::AuthStyle;
use crate::openhuman::config::Config;
use crate::openhuman::credentials::AuthService;
use crate::openhuman::inference::provider::compatible::{
    AuthStyle as CompatAuthStyle, OpenAiCompatibleProvider,
};
use crate::openhuman::inference::provider::traits::Provider;

/// Prefix for Ollama-local providers: `"ollama:<model>"`.
pub const OLLAMA_PROVIDER_PREFIX: &str = "ollama:";

/// Auth-profile storage key for a slug-keyed provider.
///
/// New writes use `"provider:<slug>"`. Lookups also try the bare `<slug>`
/// as a legacy fallback (old configs stored keys as e.g. `"openai:default"`).
pub fn auth_key_for_slug(slug: &str) -> String {
    format!("provider:{slug}")
}

/// Return the configured provider string for a named workload role.
///
pub fn provider_for_role(role: &str, config: &Config) -> String {
    let opt = match role {
        "chat" => config.chat_provider.as_deref(),
        "reasoning" => config.reasoning_provider.as_deref(),
        "agentic" => config.agentic_provider.as_deref(),
        "coding" => config.coding_provider.as_deref(),
        // `memory_provider` covers both the memory-tree extract path and
        // the summarizer sub-agent (whose definition declares
        // `hint = "summarization"`). Both are "produce a condensed
        // representation of input text" — same model class, no reason
        // for a separate config knob.
        "memory" | "summarization" => config.memory_provider.as_deref(),
        "embeddings" => config.embeddings_provider.as_deref(),
        "heartbeat" => config.heartbeat_provider.as_deref(),
        "learning" => config.learning_provider.as_deref(),
        "subconscious" => config.subconscious_provider.as_deref(),
        _ => None,
    };
    let s = opt.unwrap_or("").trim();
    // "openhuman" is the legacy sentinel written by the frontend's Default
    // routing toggle; treat it identically to empty/cloud so it falls through
    // to the primary-provider resolution below rather than erroring.
    if s.is_empty() || s == "cloud" || s == "openhuman" {
        if is_chat_role(role) {
            if let Some(default_provider) = default_provider_for_chat(config) {
                return default_provider;
            }
        }
        // When no explicit per-workload provider is set, resolve
        // primary_cloud. If it is missing or stale, fall back to the
        // first configured provider (typically the migration-seeded
        // "openai" entry).
        let primary_slug = config.primary_cloud.as_deref().and_then(|pid| {
            config
                .cloud_providers
                .iter()
                .find(|e| e.id == pid)
                .map(|e| e.slug.clone())
        });
        let resolved =
            primary_slug.or_else(|| config.cloud_providers.first().map(|e| e.slug.clone()));
        if let Some(slug) = resolved {
            format!("{slug}:")
        } else {
            String::new()
        }
    } else {
        s.to_string()
    }
}

fn is_chat_role(role: &str) -> bool {
    matches!(role, "chat" | "reasoning" | "agentic" | "coding")
}

fn primary_or_first_provider_slug(config: &Config) -> Option<String> {
    let primary_slug = config.primary_cloud.as_deref().and_then(|pid| {
        config
            .cloud_providers
            .iter()
            .find(|e| e.id == pid)
            .map(|e| e.slug.clone())
    });
    primary_slug.or_else(|| config.cloud_providers.first().map(|e| e.slug.clone()))
}

/// Resolve the model configured by Settings → AI → Chat and conversations.
///
/// `default_model` is allowed to be either a full provider string
/// (`openai:gpt-5.4`, `ollama:llama3.1:8b`) or an old bare model id. Bare
/// model ids are attached to the primary/first provider so old hand-written
/// configs continue to route to the user's external provider instead of the
/// removed OpenHuman backend.
fn default_provider_for_chat(config: &Config) -> Option<String> {
    let raw = config.default_model.as_deref()?.trim();
    if raw.is_empty() || raw == "cloud" || raw == "openhuman" {
        return None;
    }

    if let Some((slug, model)) = raw.split_once(':') {
        let slug = slug.trim();
        let model = model.trim();
        if slug.is_empty() || model.is_empty() || slug == "cloud" || slug == "openhuman" {
            return None;
        }
        if slug == "ollama"
            || config
                .cloud_providers
                .iter()
                .any(|entry| entry.slug == slug)
        {
            return Some(format!("{slug}:{model}"));
        }
        return None;
    }

    primary_or_first_provider_slug(config).map(|slug| format!("{slug}:{raw}"))
}

/// Resolve a top-level default model for a specific external-provider slug.
///
/// This is used when a provider string names a provider but leaves the model
/// blank (`"openai:"`). Older configs and a few default workload routes use
/// that shape. The first-class source is now `Config::default_model`; the
/// legacy per-provider `default_model` field is only a fallback.
pub(crate) fn default_model_for_slug(config: &Config, slug: &str) -> Option<String> {
    let raw = config.default_model.as_deref()?.trim();
    if raw.is_empty() || raw == "cloud" || raw == "openhuman" {
        return None;
    }

    if let Some((default_slug, model)) = raw.split_once(':') {
        let default_slug = default_slug.trim();
        let model = model.trim();
        if default_slug == slug && !model.is_empty() {
            return Some(model.to_string());
        }
        return None;
    }

    primary_or_first_provider_slug(config)
        .filter(|default_slug| default_slug == slug)
        .map(|_| raw.to_string())
}

/// Build a `(Provider, model)` for the given workload role.
pub fn create_chat_provider(
    role: &str,
    config: &Config,
) -> anyhow::Result<(Box<dyn Provider>, String)> {
    let s = provider_for_role(role, config);
    log::debug!(
        "[providers][chat-factory] create_chat_provider role={} resolved_string={}",
        role,
        s
    );
    create_chat_provider_from_string(role, &s, config)
}

/// Build a `(Provider, model)` from an explicit provider string and config.
///
/// See module-level grammar documentation for valid formats.
pub fn create_chat_provider_from_string(
    role: &str,
    provider: &str,
    config: &Config,
) -> anyhow::Result<(Box<dyn Provider>, String)> {
    let p = provider.trim();
    log::debug!(
        "[providers][chat-factory] create_chat_provider_from_string role={} provider={}",
        role,
        p
    );

    // "openhuman" (bare, no colon) is the legacy Default-routing sentinel
    // written by older versions of the frontend. Treat it the same as an
    // unconfigured field — no backend exists to service it.
    if p.is_empty() || p == "cloud" || p == "openhuman" {
        anyhow::bail!("No LLM provider configured. Add a provider under Settings → AI.");
    }

    // (Removed) Session gate — the OpenHuman backend session is gone
    // in the local-OAuth refactor; every workload now uses the user's
    // own external provider (or local Ollama). The gate's purpose
    // ("custom providers require an app-session JWT") no longer
    // applies in a single-user local desktop.

    // New grammar: "<slug>:<model>". Resolve cloud_providers slugs
    // FIRST so a user-added entry (e.g. slug=ollama or slug=lmstudio
    // pointing at a remote/non-default endpoint) wins over the legacy
    // `ollama:<model>` → `local_ai.base_url` special path.
    if let Some(colon_pos) = p.find(':') {
        let slug = p[..colon_pos].trim();
        let model = p[colon_pos + 1..].trim();

        if slug.is_empty() {
            anyhow::bail!(
                "[chat-factory] provider string '{}' for role '{}' has an empty slug",
                p,
                role
            );
        }

        if config.cloud_providers.iter().any(|e| e.slug == slug) {
            return make_cloud_provider_by_slug(role, slug, model, config);
        }

        // No cloud_providers entry — fall through to the legacy
        // `ollama:<model>` path that targets `local_ai.base_url`. This
        // preserves the default-config UX (Ollama models picked from
        // the Local-runtime section work even without an explicit
        // cloud_providers row).
        if slug == "ollama" {
            if model.is_empty() {
                anyhow::bail!(
                    "[chat-factory] provider string '{}' for role '{}' has an empty model — \
                     use 'ollama:<model-id>'",
                    p,
                    role
                );
            }
            return make_ollama_provider(model, config);
        }

        return make_cloud_provider_by_slug(role, slug, model, config);
    }

    // No colon: might be a bare legacy type string (e.g. "openai"). Try as
    // slug lookup with empty model — gives a clear "no entry" error rather
    // than an opaque parse failure.
    anyhow::bail!(
        "[chat-factory] unrecognised provider string '{}' for role '{}'. \
         Valid forms: ollama:<model>, <slug>:<model>. \
         Configured slugs: [{}]",
        p,
        role,
        config
            .cloud_providers
            .iter()
            .map(|e| e.slug.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Build an Ollama local provider.
fn make_ollama_provider(
    model: &str,
    config: &Config,
) -> anyhow::Result<(Box<dyn Provider>, String)> {
    let base_url = config
        .local_ai
        .base_url
        .as_deref()
        .unwrap_or("http://localhost:11434");
    // Ollama exposes an OpenAI-compatible endpoint at /v1.
    let endpoint = format!("{}/v1", base_url.trim_end_matches('/'));
    log::info!(
        "[providers][chat-factory] building ollama provider model={} endpoint_host={}",
        model,
        redact_endpoint(&endpoint)
    );
    let p = make_openai_compatible_provider_with_config(
        "ollama",
        &endpoint,
        "",
        CompatAuthStyle::None,
        &config.temperature_unsupported_models,
    )?;
    Ok((p, model.to_string()))
}

/// Look up a `cloud_providers` entry by slug and build the provider.
fn make_cloud_provider_by_slug(
    role: &str,
    slug: &str,
    model: &str,
    config: &Config,
) -> anyhow::Result<(Box<dyn Provider>, String)> {
    let entry = config.cloud_providers.iter().find(|e| e.slug == slug);

    let entry = entry.ok_or_else(|| {
        let known: Vec<&str> = config
            .cloud_providers
            .iter()
            .map(|e| e.slug.as_str())
            .collect();
        anyhow::anyhow!(
            "[chat-factory] no external provider configured for slug '{}' (role '{}') — \
             add an entry with that slug to cloud_providers in config.toml. \
             Configured slugs: [{}]",
            slug,
            role,
            known.join(", ")
        )
    })?;

    // Resolve effective model: use provided model if non-empty, else the new
    // top-level chat default for this slug, then the entry's legacy
    // default_model (if any), else empty → error.
    let effective_model = if model.trim().is_empty() {
        default_model_for_slug(config, slug)
            .or_else(|| entry.default_model.clone())
            .unwrap_or_default()
    } else {
        model.to_string()
    };
    if effective_model.is_empty() {
        anyhow::bail!(
            "[chat-factory] no model specified for provider '{}' (role '{}') — \
             set a model in the workload routing (e.g. '{slug}:gpt-4o') or add a \
             default_model to the provider config",
            slug,
            role
        );
    }

    log::info!(
        "[providers][chat-factory] role={} slug={} model={} endpoint_host={}",
        role,
        slug,
        effective_model,
        redact_endpoint(&entry.endpoint)
    );

    let key = lookup_key_for_slug(slug, config)?;

    let unsupported = &config.temperature_unsupported_models;
    match entry.auth_style {
        AuthStyle::Anthropic => {
            let p = make_openai_compatible_provider_with_config(
                slug,
                &entry.endpoint,
                &key,
                CompatAuthStyle::Anthropic,
                unsupported,
            )?;
            Ok((p, effective_model))
        }
        AuthStyle::None => {
            let p = make_openai_compatible_provider_with_config(
                slug,
                &entry.endpoint,
                "",
                CompatAuthStyle::None,
                unsupported,
            )?;
            Ok((p, effective_model))
        }
        AuthStyle::Bearer => {
            let p = make_openai_compatible_provider_with_config(
                slug,
                &entry.endpoint,
                &key,
                CompatAuthStyle::Bearer,
                unsupported,
            )?;
            Ok((p, effective_model))
        }
    }
}

/// Fetch the bearer token for a slug from the workspace `auth-profiles.json`.
///
/// Tries `provider:<slug>` first (new key format), then the bare `<slug>`
/// (legacy format where keys were stored as `"openai"`, `"anthropic"`, etc.).
/// Missing or empty keys return `Ok(String::new())` — callers treat that as
/// "no auth", which surfaces an authentication error at first call rather than
/// at factory build time.
pub fn lookup_key_for_slug(slug: &str, config: &Config) -> anyhow::Result<String> {
    let auth = AuthService::from_config(config);
    // Try new-style key first.
    let new_key = auth_key_for_slug(slug);
    if let Ok(Some(k)) = auth.get_provider_bearer_token(&new_key, None) {
        if !k.is_empty() {
            log::debug!(
                "[providers][chat-factory] auth lookup slug={} key_present=true (new-style)",
                slug
            );
            return Ok(k);
        }
    }
    // Fall back to legacy bare slug.
    let key = auth
        .get_provider_bearer_token(slug, None)
        .map_err(|e| {
            anyhow::anyhow!(
                "[chat-factory] failed to read API key for slug '{}': {}",
                slug,
                e
            )
        })?
        .unwrap_or_default();
    log::debug!(
        "[providers][chat-factory] auth lookup slug={} key_present={}",
        slug,
        !key.is_empty()
    );
    Ok(key)
}

/// Build an `OpenAiCompatibleProvider` with the given auth style.
fn make_openai_compatible_provider(
    endpoint: &str,
    api_key: &str,
    auth_style: CompatAuthStyle,
) -> anyhow::Result<Box<dyn Provider>> {
    make_openai_compatible_provider_with_config(
        "openai_compatible",
        endpoint,
        api_key,
        auth_style,
        &[],
    )
}

/// Build an `OpenAiCompatibleProvider` with auth style and temperature
/// suppression list from config.
fn make_openai_compatible_provider_with_config(
    provider_name: &str,
    endpoint: &str,
    api_key: &str,
    auth_style: CompatAuthStyle,
    temperature_unsupported_models: &[String],
) -> anyhow::Result<Box<dyn Provider>> {
    let key = if api_key.trim().is_empty() {
        None
    } else {
        Some(api_key)
    };
    Ok(Box::new(
        OpenAiCompatibleProvider::new(provider_name, endpoint, key, auth_style)
            .with_temperature_unsupported_models(temperature_unsupported_models.to_vec()),
    ))
}

/// Return a safe-to-log representation of a URL endpoint: `scheme://host` only.
pub(crate) fn redact_endpoint(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(rest) = trimmed.split_once("://") {
        let scheme = rest.0;
        let authority = rest.1.split('/').next().unwrap_or("");
        let host = authority.split('@').last().unwrap_or(authority);
        let host_no_query = host.split('?').next().unwrap_or(host);
        return format!("{}://{}", scheme, host_no_query);
    }
    "<endpoint>".to_string()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "factory_test.rs"]
mod factory_test;
