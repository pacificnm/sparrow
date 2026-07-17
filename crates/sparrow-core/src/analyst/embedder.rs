//! Embedding generation for `search_similar_incidents` (Issue 10.4).
//!
//! ## Research spike outcome (Issue 10.1)
//!
//! Ground truth checked directly against the framework source, not
//! assumed from documentation:
//!
//! - `nest_ai::AiProvider` (`core/crates/nest-ai/src/provider.rs`) has no
//!   `embed()` method — only `complete`/`stream_complete`.
//! - `nest-ai-ollama`'s `OllamaProvider`/`OllamaClient`
//!   (`modules/crates/nest-ai-ollama/src/{provider,client}.rs`) do not
//!   wrap Ollama's separate embedding HTTP endpoints (grepped both files
//!   for "embed": zero hits). This still holds as of this research spike —
//!   the gap flagged in Phase 3's research has not been closed upstream.
//!
//! Landing a framework PR against `pacificnm/nest` (same review bar as its
//! own Phases 1-3) is out of scope for what this repo can do on its own
//! timeline, so **path 2 (the stopgap) was taken**: [`OllamaEmbedder`]
//! below calls Ollama's `/api/embed` endpoint directly via
//! `nest_http_client::HttpClientService`, behind the [`Embedder`] trait —
//! so `search_similar_incidents` (Issue 10.4) depends only on the trait,
//! never on Ollama specifically, matching the same swappable-provider
//! principle this phase's `nest_ai::AiService` usage already follows. If
//! `nest-ai-ollama` ever grows a real `embed()`, only this module's
//! `OllamaEmbedder` needs to change, not its callers.
//!
//! **Confirmed embedding dimension: 768.** Verified empirically, not
//! assumed: pulled `nomic-embed-text` via `ollama pull nomic-embed-text`
//! against a real local Ollama instance and called `POST /api/embed`
//! directly — the response's `embeddings[0]` was a 768-element vector.
//! `nest-data-postgres`'s pgvector default of 1536 (OpenAI's
//! `text-embedding-3-small`) is **not** the right dimension here — Issue
//! 10.4's `resolved_incidents.embedding` column must be declared
//! `vector(768)` (see [`EMBEDDING_DIMENSION`]), not left at that default.

use async_trait::async_trait;
use nest_error::{NestError, NestResult};
use nest_http_client::HttpClientService;
use serde::{Deserialize, Serialize};

/// The confirmed output dimension of [`OllamaEmbedder`]'s default model
/// (`nomic-embed-text`) — see this module's doc comment for how this was
/// verified. Issue 10.4's `resolved_incidents` migration must declare its
/// `embedding` column as `vector(EMBEDDING_DIMENSION)`, not
/// `nest-data-postgres`'s 1536 default (OpenAI's dimension, not Ollama's).
pub const EMBEDDING_DIMENSION: usize = 768;

/// Generates a vector embedding for a piece of text.
///
/// Deliberately a separate trait from `nest_ai::AiProvider`, not a method
/// added to it here — that trait belongs to the framework
/// (`pacificnm/nest`), and it has no `embed()` (see this module's doc
/// comment). `search_similar_incidents` (Issue 10.4) takes `&dyn Embedder`,
/// never a concrete type, so swapping [`OllamaEmbedder`] for a different
/// backend later (or a real `AiProvider::embed()`, if one ever lands
/// upstream) touches only this module.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Returns `text`'s embedding vector.
    async fn embed(&self, text: &str) -> NestResult<Vec<f32>>;
}

/// Stopgap [`Embedder`] — Issue 10.1's path 2 — calling Ollama's
/// `POST /api/embed` directly, bypassing `nest_ai`/`nest-ai-ollama`
/// entirely (neither exposes embeddings; see this module's doc comment).
pub struct OllamaEmbedder {
    /// Ollama's HTTP base URL, no trailing slash (e.g.
    /// `http://127.0.0.1:11434` — the same default
    /// `nest_ai_ollama::OllamaConfig::DEFAULT_BASE_URL` uses; not
    /// reused directly from there, to avoid this module depending on
    /// `nest-ai-ollama` for anything beyond doc-comment precedent).
    base_url: String,
    model: String,
    http: HttpClientService,
}

impl OllamaEmbedder {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, http: HttpClientService) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            http,
        }
    }
}

/// `POST /api/embed` request body — Ollama's current (non-legacy)
/// embeddings endpoint; `input` accepts a single string or an array, only
/// the single-string shape is needed here.
#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
}

/// `POST /api/embed` response body. `embeddings` is a list because the
/// request's `input` can be a batch — always exactly one entry here, since
/// [`EmbedRequest::input`] is always a single string.
#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, text: &str) -> NestResult<Vec<f32>> {
        let url = format!("{}/api/embed", self.base_url);
        let response: EmbedResponse = self
            .http
            .post_json(
                &url,
                &EmbedRequest {
                    model: &self.model,
                    input: text,
                },
            )
            .await?;

        response
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| NestError::unknown("Ollama /api/embed returned no embeddings"))
    }
}

#[cfg(test)]
mod tests {
    use nest_http_client::HttpClientConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn embedding_dimension_matches_the_empirically_confirmed_value() {
        assert_eq!(EMBEDDING_DIMENSION, 768);
    }

    #[tokio::test]
    async fn embed_posts_to_the_embed_endpoint_and_parses_the_vector() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "nomic-embed-text",
                "embeddings": [vec![0.1_f32; EMBEDDING_DIMENSION]],
            })))
            .mount(&server)
            .await;

        let http = HttpClientService::new(HttpClientConfig::default()).expect("http client");
        let embedder = OllamaEmbedder::new(server.uri(), "nomic-embed-text", http);

        let embedding = embedder
            .embed("disk usage exceeded 90 percent on host web-01")
            .await
            .expect("embed should succeed");

        assert_eq!(embedding.len(), EMBEDDING_DIMENSION);
        assert_eq!(embedding[0], 0.1);
    }

    #[tokio::test]
    async fn embed_fails_when_the_response_has_no_embeddings() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "nomic-embed-text",
                "embeddings": [],
            })))
            .mount(&server)
            .await;

        let http = HttpClientService::new(HttpClientConfig::default()).expect("http client");
        let embedder = OllamaEmbedder::new(server.uri(), "nomic-embed-text", http);

        let error = embedder.embed("empty response").await.unwrap_err();
        assert!(error.to_string().contains("no embeddings"));
    }
}
