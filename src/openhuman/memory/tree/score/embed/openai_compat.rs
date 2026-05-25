//! OpenAI-compatible embedder for Phase 4 (#710).
//!
//! Posts `{model, input}` to `{endpoint}/v1/embeddings` (or custom path)
//! and expects `{"data": [{"embedding": [f32; EMBEDDING_DIM]}]}` back.
//! Designed for OpenAI-compatible endpoints like LiteLLM proxy.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{Embedder, EMBEDDING_DIM};

/// Default request timeout (10 seconds).
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// HTTP client wrapping an OpenAI-compatible endpoint + model pair.
#[derive(Clone)]
pub struct OpenAiCompatEmbedder {
    endpoint: String,
    model: String,
    api_key: String,
    timeout: Duration,
    client: reqwest::Client,
}

impl OpenAiCompatEmbedder {
    /// Build a new embedder. `endpoint` is trimmed of trailing slashes.
    /// Supports `custom:` prefix (stripped) and OpenAI-compatible paths.
    pub fn new(endpoint: String, model: String, api_key: String, timeout_ms: u64) -> Self {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        let endpoint = if endpoint.starts_with("custom:") {
            endpoint
                .strip_prefix("custom:")
                .unwrap_or(&endpoint)
                .to_string()
        } else {
            endpoint
        };
        let model = if model.trim().is_empty() {
            "text-embedding-ada-002".to_string()
        } else {
            model.trim().to_string()
        };
        let timeout_ms = if timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            timeout_ms
        };
        let timeout = Duration::from_millis(timeout_ms);
        let client = reqwest::Client::builder()
            .connect_timeout(timeout)
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|e| {
                log::warn!("[memory_tree::embed::openai_compat] failed to build client: {e}");
                reqwest::Client::new()
            });
        log::debug!(
            "[memory_tree::embed::openai_compat] created endpoint={endpoint} model={model} timeout_ms={timeout_ms}"
        );
        Self {
            endpoint,
            model,
            api_key,
            timeout,
            client,
        }
    }

    /// Constructs the final URL for the embeddings endpoint.
    fn embed_url(&self) -> String {
        // If endpoint already ends with /v1 or similar, just append /embeddings
        if self.endpoint.contains("/v1") {
            format!("{}/embeddings", self.endpoint.trim_end_matches('/'))
        } else {
            format!("{}/v1/embeddings", self.endpoint.trim_end_matches('/'))
        }
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

#[async_trait]
impl Embedder for OpenAiCompatEmbedder {
    fn name(&self) -> &'static str {
        "openai_compat"
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        log::debug!(
            "[memory_tree::embed::openai_compat] embed endpoint={} model={} bytes={}",
            self.endpoint,
            self.model,
            text.len()
        );
        let req = EmbedRequest {
            model: &self.model,
            input: text,
        };
        let mut request = self.client.post(self.embed_url()).json(&req);

        // Add Authorization header if api_key is provided
        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let resp = request.send().await.with_context(|| {
            format!(
                "openai_compat embeddings request failed (endpoint: {})",
                self.embed_url()
            )
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "openai_compat embeddings failed status={status} body={}",
                body.trim()
            );
        }

        let payload: EmbedResponse = resp
            .json()
            .await
            .context("openai_compat embeddings response parse failed")?;

        if payload.data.is_empty() {
            anyhow::bail!("openai_compat embeddings returned empty data array");
        }

        let embedding = &payload.data[0].embedding;
        if embedding.len() != EMBEDDING_DIM {
            // Log warning but don't fail - some models have different dimensions
            log::warn!(
                "openai_compat embeddings returned {} dims, expected {} (model={})",
                embedding.len(),
                EMBEDDING_DIM,
                self.model
            );
        }

        Ok(embedding.clone())
    }
}
