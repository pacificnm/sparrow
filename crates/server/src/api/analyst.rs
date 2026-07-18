//! `POST /api/analyst/run`.
//!
//! Same `RequestContext` ground truth as every other `api/` handler
//! (verified against `nest-http-serve/src/context.rs` directly):
//! `ctx.json::<T>()` deserializes the POST body, rejecting an empty body or
//! invalid JSON with a `ServeError` rather than panicking. No
//! service-access method exists, so `AiService`/`PgPool`/`Embedder` are
//! captured by the route closure `routes()` builds.

use std::sync::Arc;

use nest_ai::AiService;
use nest_error::{NestError, NestResult};
use nest_http_serve::{HttpResult, Json, RequestContext, RouteGroup};
use serde::{Deserialize, Serialize};
use sparrow_core::analyst::embedder::Embedder;
use sparrow_core::analyst::r#loop::{run_analysis, AnalysisMode};
use sqlx::PgPool;

use crate::api::problems::fetch_open_problems;

/// Sets the model's persona/goal — the four tools it can call are attached
/// separately by `run_analysis` itself (`tool_definitions()`), so this
/// doesn't need to enumerate them.
const SYSTEM_PROMPT: &str = "You are Sparrow's AI Health Analyst. You have \
tools available to inspect host status, metric history, and active or past \
Problems. Use them to investigate before answering. Be concise and specific, \
and mention host_id/metric key values you actually observed.";

/// Mirrors Phase 10's own `RunAnalysisRequest` sketch exactly
/// (`docs/plans/phase-10-ai-health-analyst.md`, "API" section) — this is
/// the same shape `desktop/src-tauri`'s `run_analysis` command (Issue
/// 11.1) sends, not a shape invented independently here.
#[derive(Debug, Deserialize)]
struct RunAnalysisRequest {
    host_id: Option<String>,
    /// Free-form question. If absent and `host_id` is present, this is the
    /// "explain this Problem" quick action — synthesize a default prompt
    /// from that host's currently open Problem(s) rather than requiring
    /// the caller to phrase a question.
    question: Option<String>,
    #[serde(default)]
    mode: AnalysisMode,
}

/// The response shape Issue 11.1's desktop client assumes — `{"response":
/// "..."}`, not a bare JSON string, since Phase 10's spec only said
/// "return the response text as JSON" without a concrete struct.
#[derive(Serialize)]
struct RunAnalysisResponse {
    response: String,
}

/// Builds the `/api/analyst/run` route.
pub fn routes(ai: AiService, pool: PgPool, embedder: Arc<dyn Embedder>) -> RouteGroup {
    RouteGroup::new("/api").post("/analyst/run", move |ctx| {
        let ai = ai.clone();
        let pool = pool.clone();
        let embedder = Arc::clone(&embedder);
        async move { run_analysis_handler(ctx, ai, pool, embedder).await }
    })
}

async fn run_analysis_handler(
    ctx: RequestContext,
    ai: AiService,
    pool: PgPool,
    embedder: Arc<dyn Embedder>,
) -> HttpResult {
    let request: RunAnalysisRequest = ctx.json()?;
    let user_prompt = build_user_prompt(&pool, &request).await?;

    let response = run_analysis(
        &ai,
        &pool,
        embedder.as_ref(),
        SYSTEM_PROMPT,
        &user_prompt,
        request.mode,
    )
    .await?;

    Json(RunAnalysisResponse { response }).into_response()
}

/// Resolves the user prompt per `RunAnalysisRequest::question`'s doc
/// comment: a free-form question takes priority; otherwise, given a
/// `host_id`, synthesize a prompt from that host's currently open
/// Problem(s); with neither, there's nothing to analyze.
async fn build_user_prompt(pool: &PgPool, request: &RunAnalysisRequest) -> NestResult<String> {
    if let Some(question) = &request.question {
        return Ok(question.clone());
    }

    let host_id = request.host_id.as_deref().ok_or_else(|| {
        NestError::validation("run_analysis requires either `question` or `host_id`")
    })?;

    let problems = fetch_open_problems(pool, Some(host_id))
        .await
        .map_err(|error| NestError::unknown(format!("failed to fetch open problems: {error}")))?;

    if problems.is_empty() {
        return Ok(format!(
            "Host {host_id} currently has no open problems. Confirm there is nothing to \
             explain and say so plainly."
        ));
    }

    let mut prompt = format!("Explain the following open problem(s) for host {host_id}:\n");
    for problem in &problems {
        prompt.push_str(&format!(
            "- problem_id={}, rule_id={}, last_value={}, opened_at={}\n",
            problem.id, problem.rule_id, problem.last_value, problem.opened_at
        ));
    }
    Ok(prompt)
}

#[cfg(test)]
mod tests {
    use nest_ai::{AiError, AiProvider, AiResult, CompletionResponse};

    use super::*;

    /// Never actually invoked — used only to build a valid `AiService` for
    /// the route-registration test, which doesn't send any request.
    struct UnusedProvider;

    #[async_trait::async_trait]
    impl AiProvider for UnusedProvider {
        fn provider_id(&self) -> &'static str {
            "unused"
        }

        async fn complete(
            &self,
            _request: nest_ai::CompletionRequest,
        ) -> AiResult<CompletionResponse> {
            Err(AiError::invalid_input("not used in this test"))
        }
    }

    /// Never actually invoked by these tests either — no test here reaches
    /// `search_similar_incidents`.
    struct UnusedEmbedder;

    #[async_trait::async_trait]
    impl Embedder for UnusedEmbedder {
        async fn embed(&self, _text: &str) -> nest_error::NestResult<Vec<f32>> {
            Ok(vec![
                0.0;
                sparrow_core::analyst::embedder::EMBEDDING_DIMENSION
            ])
        }
    }

    #[tokio::test]
    async fn routes_registers_the_expected_pattern_and_method() {
        use nest_http_serve::{HttpMethod, RouteRegistry};

        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");
        let ai = AiService::new(Arc::new(UnusedProvider));

        let mut registry = RouteRegistry::new();
        registry.add_group(routes(ai, pool, Arc::new(UnusedEmbedder)));

        let found: Vec<(HttpMethod, &str)> = registry
            .routes()
            .iter()
            .map(|route| (route.method, route.pattern.as_str()))
            .collect();

        assert_eq!(found, vec![(HttpMethod::Post, "/api/analyst/run")]);
    }

    /// A scripted `AiProvider` that answers directly (no tool calls) with
    /// whatever content it's given — enough to prove the handler's
    /// request-parsing/prompt-building/response-shaping wiring without a
    /// live model, matching `analyst::r#loop::tests::FakeProvider`'s
    /// approach from Issue 10.3.
    struct EchoProvider {
        answer: String,
    }

    #[async_trait::async_trait]
    impl AiProvider for EchoProvider {
        fn provider_id(&self) -> &'static str {
            "echo"
        }

        async fn complete(
            &self,
            _request: nest_ai::CompletionRequest,
        ) -> AiResult<CompletionResponse> {
            Ok(CompletionResponse {
                model: "echo".to_string(),
                content: self.answer.clone(),
                done: true,
                tool_calls: Vec::new(),
                metrics: None,
            })
        }
    }

    fn test_ai(answer: &str) -> AiService {
        AiService::new(Arc::new(EchoProvider {
            answer: answer.to_string(),
        }))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_user_prompt_uses_the_question_verbatim_when_present() {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");
        let request = RunAnalysisRequest {
            host_id: Some("host-1".to_string()),
            question: Some("why is disk usage high?".to_string()),
            mode: AnalysisMode::Quick,
        };

        let prompt = build_user_prompt(&pool, &request)
            .await
            .expect("build_user_prompt should succeed");

        assert_eq!(prompt, "why is disk usage high?");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_user_prompt_requires_question_or_host_id() {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");
        let request = RunAnalysisRequest {
            host_id: None,
            question: None,
            mode: AnalysisMode::Quick,
        };

        let error = build_user_prompt(&pool, &request).await.unwrap_err();
        assert!(error.to_string().contains("question"));
    }

    // --- Docker-backed tests below: real seeded Postgres data plus a
    // fake AiProvider (no live Ollama/Claude needed), per this issue's
    // acceptance ("both endpoints return real data against a seeded
    // server").

    use nest_data::DataModule;
    use nest_data_postgres::{PostgresConfig, PostgresDataModule};
    use sparrow_core::storage::HostRegistry;
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt};

    struct TestDb {
        _container: ContainerAsync<GenericImage>,
        pool: PgPool,
    }

    async fn start_postgres_with_schema() -> TestDb {
        let container = GenericImage::new("pgvector/pgvector", "pg16")
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .start()
            .await
            .expect("failed to start postgres testcontainer");
        let host = container.get_host().await.expect("container host");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("container port");
        let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

        nest_core::AppBuilder::new()
            .module(DataModule)
            .module(
                PostgresDataModule::new(PostgresConfig::new(url.clone()))
                    .with_migrations(sparrow_core::migrations::all_migrations()),
            )
            .build()
            .expect("app with postgres migrations");

        let pool = PgPool::connect(&url).await.expect("fresh postgres pool");
        TestDb {
            _container: container,
            pool,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_user_prompt_synthesizes_a_prompt_from_open_problems() {
        let db = start_postgres_with_schema().await;
        let host_id = "analyst-prompt-host";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "test-host")
            .await
            .expect("seed host");
        let rule_id: i64 = sqlx::query_scalar(
            "INSERT INTO rules (host_id, item_key, operator, threshold, severity)
             VALUES ($1, 'cpu.usage_percent', 'greater_than', 90.0, 'warning')
             RETURNING id",
        )
        .bind(host_id)
        .fetch_one(&db.pool)
        .await
        .expect("insert rule");
        sqlx::query(
            "INSERT INTO problems (rule_id, host_id, status, opened_at, last_value)
             VALUES ($1, $2, 'open', 0, 95.0)",
        )
        .bind(rule_id)
        .bind(host_id)
        .execute(&db.pool)
        .await
        .expect("insert open problem");

        let request = RunAnalysisRequest {
            host_id: Some(host_id.to_string()),
            question: None,
            mode: AnalysisMode::Quick,
        };
        let prompt = build_user_prompt(&db.pool, &request)
            .await
            .expect("build_user_prompt should succeed");

        assert!(prompt.contains(host_id));
        assert!(prompt.contains(&rule_id.to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_user_prompt_reports_no_open_problems_when_there_are_none() {
        let db = start_postgres_with_schema().await;
        let host_id = "analyst-no-problems-host";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "test-host")
            .await
            .expect("seed host");

        let request = RunAnalysisRequest {
            host_id: Some(host_id.to_string()),
            question: None,
            mode: AnalysisMode::Quick,
        };
        let prompt = build_user_prompt(&db.pool, &request)
            .await
            .expect("build_user_prompt should succeed");

        assert!(prompt.contains("no open problems"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_analysis_http_endpoint_returns_the_models_response() {
        use nest_http_serve::HttpServer;

        let db = start_postgres_with_schema().await;
        let ai = test_ai("all systems normal");

        let server = HttpServer::builder()
            .routes(routes(ai, db.pool.clone(), Arc::new(UnusedEmbedder)))
            .spawn()
            .await
            .expect("http server should spawn");

        let http_client = reqwest::Client::new();
        let response = http_client
            .post(format!("{}/api/analyst/run", server.base_url()))
            .json(&serde_json::json!({
                "host_id": null,
                "question": "how is everything?",
                "mode": "quick"
            }))
            .send()
            .await
            .expect("POST /api/analyst/run should succeed");
        assert!(response.status().is_success());

        let body: serde_json::Value = response.json().await.expect("valid JSON body");
        assert_eq!(body["response"], "all systems normal");

        server.shutdown().await;
    }
}
