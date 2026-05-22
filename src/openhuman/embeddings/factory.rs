//! Factory functions for creating embedding providers.

use std::sync::Arc;

use super::provider_trait::EmbeddingProvider;
use super::{NoopEmbedding, OllamaEmbedding, OpenAiEmbedding};

/// Creates an embedding provider based on the specified name and configuration.
///
/// Supported provider names:
/// - `"ollama"` → local Ollama server
/// - `"openai"` → OpenAI API
/// - `"custom:<url>"` → OpenAI-compatible endpoint
/// - `"none"` → no-op (keyword-only search, no embeddings)
///
/// Returns an error for unrecognised provider names so configuration
/// mistakes surface immediately rather than silently degrading to
/// keyword-only search.
pub fn create_embedding_provider(
    provider: &str,
    model: &str,
    dims: usize,
) -> anyhow::Result<Box<dyn EmbeddingProvider>> {
    match provider {
        "ollama" => {
            let base_url = crate::openhuman::inference::local::ollama_base_url();
            Ok(Box::new(OllamaEmbedding::try_new(&base_url, model, dims)?))
        }
        "openai" => Ok(Box::new(OpenAiEmbedding::new(
            "https://api.openai.com",
            "",
            model,
            dims,
        ))),
        name if name.starts_with("custom:") => {
            let base_url = name.strip_prefix("custom:").unwrap_or("");
            Ok(Box::new(OpenAiEmbedding::new(base_url, "", model, dims)))
        }
        "none" => Ok(Box::new(NoopEmbedding)),
        unknown => Err(anyhow::anyhow!(
            "unknown embedding provider: \"{unknown}\". \
             Supported: \"ollama\", \"openai\", \"custom:<url>\", \"none\""
        )),
    }
}

/// Returns the default local Ollama-backed embedding provider.
pub fn default_embedding_provider() -> Arc<dyn EmbeddingProvider> {
    Arc::new(OllamaEmbedding::default())
}

/// Returns the local Ollama-backed embedding provider. Only used when the
/// caller has explicitly opted into local-only embeddings.
pub fn default_local_embedding_provider() -> Arc<dyn EmbeddingProvider> {
    Arc::new(OllamaEmbedding::default())
}
