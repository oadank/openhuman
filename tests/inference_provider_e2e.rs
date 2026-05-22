//! Inference provider end-to-end tests using wiremock.
//!
//! These tests spin up a wiremock HTTP server on a random port and verify
//! that `OpenAiCompatibleProvider` sends correct request bodies and correctly
//! interprets responses for the major provider shapes (OpenAI-compat,
//! Anthropic auth, streaming, temperature suppression, Ollama endpoint).
//!
//! The `/v1/chat/completions` and `/v1/models` HTTP endpoint tests verify the
//! full axum router layer (auth middleware + provider routing) end-to-end.
//!
//! Tests 15–16 exercise the **full factory chain**: Config + auth-profile key
//! lookup → `create_chat_provider` → `OpenAiCompatibleProvider`. Test 16 is a
//! live endpoint test (marked `#[ignore]`) — run it with:
//!
//! ```text
//! OPENHUMAN_TEST_CUSTOM_KEY=<key> cargo test --test inference_provider_e2e \
//!     custom_provider_live -- --include-ignored
//! ```

use std::sync::{Mutex, OnceLock};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::ServiceExt;
use wiremock::matchers::{header as wm_header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use openhuman_core::core::auth::{init_rpc_token, CORE_TOKEN_ENV_VAR};
use openhuman_core::core::jsonrpc::build_core_http_router;
use openhuman_core::openhuman::config::schema::cloud_providers::{
    AuthStyle as ConfigAuthStyle, CloudProviderCreds,
};
use openhuman_core::openhuman::config::Config;
use openhuman_core::openhuman::credentials::AuthService;
use openhuman_core::openhuman::inference::provider::compatible::{
    AuthStyle, OpenAiCompatibleProvider,
};
use openhuman_core::openhuman::inference::provider::factory::create_chat_provider;
use openhuman_core::openhuman::inference::provider::traits::{ChatMessage, Provider};

// ── Environment serialisation lock ───────────────────────────────────────────
//
// Tests that mutate OPENHUMAN_WORKSPACE or OPENHUMAN_CORE_TOKEN must acquire
// this lock first to prevent races when cargo runs tests in parallel threads
// within the same process.

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static RPC_AUTH_INIT: OnceLock<()> = OnceLock::new();

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    let m = ENV_LOCK.get_or_init(|| Mutex::new(()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

const TEST_RPC_TOKEN: &str = "inference-provider-e2e-token";

fn ensure_rpc_auth() {
    RPC_AUTH_INIT.get_or_init(|| {
        // SAFETY: test-only, serialised by OnceLock.
        unsafe { std::env::set_var(CORE_TOKEN_ENV_VAR, TEST_RPC_TOKEN) };
        let tmp = tempdir().expect("tempdir");
        init_rpc_token(tmp.path()).expect("init rpc auth token");
        // Keep tmp alive for the process duration by leaking it — the token
        // file must remain readable for all subsequent auth checks.
        std::mem::forget(tmp);
    });
}

// ── Canned OpenAI-compatible response body ────────────────────────────────────

fn openai_chat_response(content: &str) -> Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1_700_000_000_u64,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 10, "total_tokens": 15 }
    })
}

// ── Helper: build an env-isolated Config pointing at tempdir ─────────────────

/// Sets OPENHUMAN_WORKSPACE to `dir` and returns an `EnvVarGuard` that
/// restores the previous value on drop.  Must be called under `env_lock()`.
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, val: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: caller holds env_lock().
        unsafe { std::env::set_var(key, val) };
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            // SAFETY: caller's env_lock guard is still alive during drop.
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

// ── Test 1: OpenAI-compat chat returns canned text ───────────────────────────

#[tokio::test]
async fn openai_compat_chat_returns_canned_text() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("Hello!")))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        "test",
        &format!("{}/v1", server.uri()),
        Some("test-key"),
        AuthStyle::Bearer,
    );

    let messages = vec![ChatMessage::user("hi")];
    let result = provider
        .chat_with_history(&messages, "gpt-4o-mini", 0.7)
        .await
        .expect("chat_with_history should succeed");

    assert_eq!(result, "Hello!");
}

// ── Test 2: Temperature present for normal model ──────────────────────────────

#[tokio::test]
async fn openai_compat_temperature_present_for_normal_model() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("ok")))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        "test",
        &format!("{}/v1", server.uri()),
        Some("key"),
        AuthStyle::Bearer,
    );

    provider
        .chat_with_history(&[ChatMessage::user("hi")], "gpt-4o-mini", 0.7)
        .await
        .expect("should succeed");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(
        body.get("temperature").is_some(),
        "temperature should be present for gpt-4o-mini; body={body}"
    );
    assert_eq!(body["temperature"].as_f64().unwrap(), 0.7);
}

// ── Test 3: Temperature omitted for o1 models ────────────────────────────────

#[tokio::test]
async fn openai_compat_omits_temperature_for_o1_models() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("done")))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        "test",
        &format!("{}/v1", server.uri()),
        Some("key"),
        AuthStyle::Bearer,
    )
    .with_temperature_unsupported_models(vec!["o1*".to_string()]);

    provider
        .chat_with_history(&[ChatMessage::user("reason")], "o1-preview", 0.7)
        .await
        .expect("should succeed");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(
        body.get("temperature").is_none(),
        "temperature must be absent for o1-preview; body={body}"
    );
    // Response should still be returned correctly.
}

// ── Test 4: Temperature omitted for gpt-5 models ─────────────────────────────

#[tokio::test]
async fn openai_compat_omits_temperature_for_gpt5_models() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("done")))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        "test",
        &format!("{}/v1", server.uri()),
        Some("key"),
        AuthStyle::Bearer,
    )
    .with_temperature_unsupported_models(vec![
        "o1*".to_string(),
        "o3*".to_string(),
        "o4*".to_string(),
        "gpt-5*".to_string(),
    ]);

    for model in &["gpt-5", "gpt-5-turbo", "o3-mini", "o4-preview"] {
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("done")))
            .mount(&server)
            .await;

        provider
            .chat_with_history(&[ChatMessage::user("test")], model, 0.7)
            .await
            .expect("should succeed");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "model={model}");
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(
            body.get("temperature").is_none(),
            "temperature must be absent for model={model}; body={body}"
        );
    }
}

// ── Test 5: Anthropic auth style ─────────────────────────────────────────────

#[tokio::test]
async fn openai_compat_anthropic_auth_uses_x_api_key_header() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(wm_header("x-api-key", "sk-ant-test"))
        .and(wm_header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("hi")))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        "anthropic",
        &format!("{}/v1", server.uri()),
        Some("sk-ant-test"),
        AuthStyle::Anthropic,
    );

    let result = provider
        .chat_with_history(&[ChatMessage::user("hello")], "claude-3-haiku", 0.5)
        .await
        .expect("Anthropic auth chat should succeed");

    assert_eq!(result, "hi");

    // Verify Bearer header was NOT sent.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let auth = requests[0].headers.get("authorization");
    assert!(
        auth.is_none(),
        "Authorization header must NOT be set for Anthropic auth; found {:?}",
        auth
    );
}

// ── Test 6: Streaming response returns ordered deltas ────────────────────────

#[tokio::test]
async fn openai_compat_streaming_returns_ordered_deltas() {
    let server = MockServer::start().await;

    let sse_body = concat!(
        "data: {\"id\":\"x\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"!\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        "test",
        &format!("{}/v1", server.uri()),
        Some("key"),
        AuthStyle::Bearer,
    );

    // stream_chat_with_system is the implemented streaming method on this provider.
    let options = openhuman_core::openhuman::inference::provider::traits::StreamOptions::new(true);
    use futures_util::StreamExt;
    let mut stream = provider.stream_chat_with_system(
        Some("You are helpful."),
        "Say Hello!",
        "gpt-4o-mini",
        0.7,
        options,
    );

    let mut deltas = Vec::new();
    while let Some(result) = stream.next().await {
        let chunk = result.expect("stream chunk should be Ok");
        if !chunk.delta.is_empty() {
            deltas.push(chunk.delta);
        }
    }

    let combined = deltas.join("");
    assert_eq!(
        combined, "Hello!",
        "combined stream deltas should equal 'Hello!'; got '{combined}'"
    );
}

// ── Test 7: Ollama endpoint shape ────────────────────────────────────────────

#[tokio::test]
async fn ollama_compat_chat_via_openai_v1_endpoint() {
    let server = MockServer::start().await;

    // Ollama via OpenAI-compat /v1 endpoint — wiremock pretends to be Ollama.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("Bonjour!")))
        .mount(&server)
        .await;

    // Factory builds Ollama provider via OpenAiCompatibleProvider at /v1.
    let base = server.uri();
    let endpoint = format!("{}/v1", base.trim_end_matches('/'));
    let provider = OpenAiCompatibleProvider::new("ollama", &endpoint, None, AuthStyle::None);

    let result = provider
        .chat_with_history(&[ChatMessage::user("Bonjour?")], "llama3", 0.7)
        .await
        .expect("Ollama compat chat should succeed");

    assert_eq!(result, "Bonjour!");
}

// ── Test 8: /v1/chat/completions HTTP endpoint — unauthorized ─────────────────

#[tokio::test]
async fn http_endpoint_chat_completions_no_bearer_returns_401() {
    let _lock = env_lock();
    ensure_rpc_auth();

    let body = json!({
        "model": "ollama:llama3",
        "messages": [{ "role": "user", "content": "hello" }]
    });
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = build_core_http_router(false).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── Test 9: /v1/models — unauthorized ────────────────────────────────────────

#[tokio::test]
async fn http_endpoint_models_no_bearer_returns_401() {
    let _lock = env_lock();
    ensure_rpc_auth();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let resp = build_core_http_router(false).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── Test 10: /v1/models with bearer returns non-empty list ───────────────────

#[tokio::test]
async fn http_endpoint_models_with_bearer_returns_model_list() {
    let _lock = env_lock();
    ensure_rpc_auth();

    let tmp = tempdir().expect("tempdir");
    let _workspace_guard = EnvGuard::set("OPENHUMAN_WORKSPACE", tmp.path().to_str().unwrap());

    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/models")
        .header(header::AUTHORIZATION, format!("Bearer {TEST_RPC_TOKEN}"))
        .body(Body::empty())
        .unwrap();

    let resp = build_core_http_router(false).oneshot(req).await.unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "401 must not fire when bearer is present"
    );
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "403 must not fire when bearer is present"
    );

    if resp.status().is_success() {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let models = json.get("data").and_then(Value::as_array);
        if let Some(list) = models {
            assert!(
                !list.is_empty(),
                "/v1/models should return at least one model"
            );
        }
    }
}

// ── Test 11: /v1/chat/completions with bearer passes auth ────────────────────

#[tokio::test]
async fn http_endpoint_chat_completions_with_bearer_passes_auth() {
    let _lock = env_lock();
    ensure_rpc_auth();

    let body = json!({
        "model": "ollama:llama3",
        "messages": [{ "role": "user", "content": "ping" }],
        "stream": false
    });
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {TEST_RPC_TOKEN}"))
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = build_core_http_router(false).oneshot(req).await.unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "401 must not fire when bearer is present"
    );
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "403 must not fire when bearer is present"
    );
}

// ── Test 12: Request model field is preserved ─────────────────────────────────

#[tokio::test]
async fn openai_compat_request_body_contains_correct_model() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("ok")))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        "test",
        &format!("{}/v1", server.uri()),
        Some("key"),
        AuthStyle::Bearer,
    );

    provider
        .chat_with_history(&[ChatMessage::user("hi")], "claude-3-sonnet", 0.5)
        .await
        .expect("should succeed");

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"].as_str().unwrap(), "claude-3-sonnet");
}

// ── Test 13: Bearer token is sent in Authorization header ────────────────────

#[tokio::test]
async fn openai_compat_bearer_auth_sends_authorization_header() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(wm_header("authorization", "Bearer secret-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_chat_response("ok")))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new(
        "test",
        &format!("{}/v1", server.uri()),
        Some("secret-key"),
        AuthStyle::Bearer,
    );

    let result = provider
        .chat_with_history(&[ChatMessage::user("hi")], "gpt-4o", 0.7)
        .await
        .expect("should succeed");

    assert_eq!(result, "ok");
}

// ── Test 14: temperature_for_model helper ────────────────────────────────────

#[test]
fn temperature_helper_suppresses_o1_by_default_config() {
    use openhuman_core::openhuman::inference::provider::temperature::temperature_for_model;

    let config = Config::default();

    // Normal model → temperature returned
    assert_eq!(
        temperature_for_model("gpt-4o-mini", 0.7, &config),
        Some(0.7)
    );
    assert_eq!(
        temperature_for_model("claude-3-sonnet", 0.5, &config),
        Some(0.5)
    );

    // o1/o3/o4/gpt-5 → temperature suppressed
    assert_eq!(temperature_for_model("o1-preview", 0.7, &config), None);
    assert_eq!(temperature_for_model("o3-mini", 0.7, &config), None);
    assert_eq!(temperature_for_model("o4-turbo", 0.7, &config), None);
    assert_eq!(temperature_for_model("gpt-5-turbo", 0.7, &config), None);
}

// ── Test 15: full factory chain — custom Bearer provider via wiremock ─────────
//
// Exercises: Config → provider_for_role → create_chat_provider →
//   AuthService key lookup → OpenAiCompatibleProvider (Bearer) → HTTP call.
// Uses wiremock so no live API key is needed.

#[tokio::test]
async fn custom_bearer_provider_factory_full_chain() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(wm_header("authorization", "Bearer test-custom-key-15"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(openai_chat_response("Custom provider works!")),
        )
        .mount(&server)
        .await;

    let _lock = env_lock();
    let tmp = tempdir().expect("tempdir");
    let _workspace_guard = EnvGuard::set("OPENHUMAN_WORKSPACE", tmp.path().to_str().unwrap());

    let mut config = Config::default();
    config.workspace_dir = tmp.path().to_path_buf();
    config.config_path = tmp.path().join("config.toml");
    config.chat_provider = Some("custom:gpt-5.4-mini".to_string());
    config.cloud_providers.push(CloudProviderCreds {
        id: "p_custom_test".to_string(),
        slug: "custom".to_string(),
        label: "Custom".to_string(),
        endpoint: format!("{}/v1", server.uri()),
        auth_style: ConfigAuthStyle::Bearer,
        ..Default::default()
    });

    // Store the API key in auth-profiles.json under the workspace dir.
    let auth = AuthService::from_config(&config);
    auth.store_provider_token(
        "provider:custom",
        "default",
        "test-custom-key-15",
        Default::default(),
        true,
    )
    .expect("store provider token");

    // Build provider via the full factory (same path the core uses at runtime).
    let (provider, model) =
        create_chat_provider("chat", &config).expect("create_chat_provider must succeed");
    assert_eq!(model, "gpt-5.4-mini");

    let result = provider
        .chat_with_system(None, "Are you working?", &model, 0.7)
        .await
        .expect("chat_with_system should succeed");
    assert_eq!(result, "Custom provider works!");

    // Verify request shape.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "exactly one HTTP request should be made");
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["model"].as_str().unwrap(),
        "gpt-5.4-mini",
        "model field in request body must match"
    );
    assert!(
        body.get("messages").is_some(),
        "messages array must be present"
    );
}

// ── Test 16: live endpoint validation (ignored by default) ────────────────────
//
// Hits the real custom OpenAI-compatible endpoint to confirm the full inference
// path works without rebuilding the server.
//
// Run manually:
//   OPENHUMAN_TEST_CUSTOM_KEY=<key> \
//     cargo test --test inference_provider_e2e custom_provider_live -- --include-ignored
//
// The test also accepts a custom base URL via OPENHUMAN_TEST_CUSTOM_URL
// (default: https://api.theclawbay.com/v1) and model via
// OPENHUMAN_TEST_CUSTOM_MODEL (default: gpt-5.4-mini).

#[tokio::test]
#[ignore = "live API — set OPENHUMAN_TEST_CUSTOM_KEY and run with --include-ignored"]
async fn custom_provider_live_chat_roundtrip() {
    let api_key = std::env::var("OPENHUMAN_TEST_CUSTOM_KEY").unwrap_or_default();
    if api_key.is_empty() {
        eprintln!("[skip] OPENHUMAN_TEST_CUSTOM_KEY is not set");
        return;
    }

    let base_url = std::env::var("OPENHUMAN_TEST_CUSTOM_URL")
        .unwrap_or_else(|_| "https://api.theclawbay.com/v1".to_string());
    let model_id =
        std::env::var("OPENHUMAN_TEST_CUSTOM_MODEL").unwrap_or_else(|_| "gpt-5.4-mini".to_string());

    let _lock = env_lock();
    let tmp = tempdir().expect("tempdir");
    let _workspace_guard = EnvGuard::set("OPENHUMAN_WORKSPACE", tmp.path().to_str().unwrap());

    let mut config = Config::default();
    config.workspace_dir = tmp.path().to_path_buf();
    config.config_path = tmp.path().join("config.toml");
    config.chat_provider = Some(format!("custom:{model_id}"));
    config.cloud_providers.push(CloudProviderCreds {
        id: "p_custom_live".to_string(),
        slug: "custom".to_string(),
        label: "Custom".to_string(),
        endpoint: base_url.clone(),
        auth_style: ConfigAuthStyle::Bearer,
        ..Default::default()
    });

    let auth = AuthService::from_config(&config);
    auth.store_provider_token(
        "provider:custom",
        "default",
        &api_key,
        Default::default(),
        true,
    )
    .expect("store provider token");

    let (provider, model) =
        create_chat_provider("chat", &config).expect("create_chat_provider must succeed");
    assert_eq!(model, model_id);

    let result = provider
        .chat_with_system(
            Some("You are a test harness. Reply with exactly one word: pong"),
            "ping",
            &model,
            0.7,
        )
        .await
        .expect("live chat should succeed");

    assert!(
        !result.is_empty(),
        "live chat response must be non-empty; endpoint={base_url} model={model}"
    );
    eprintln!("[live] endpoint={base_url} model={model} response={result:?}");
}
