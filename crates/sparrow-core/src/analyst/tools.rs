//! Tool definitions and dispatch for the AI Health Analyst's agent loop
//! (Issue 10.3's `loop.rs` calls [`execute_tool`] after each model turn
//! that requests one, and [`tool_definitions`] on every request so the
//! model always sees the same four tools).
//!
//! Depends only on `nest_ai` (`ToolDefinition`/`ToolCall`) — never
//! `nest_ai_ollama`/`nest_ai_claude` directly, here or anywhere else in
//! `analyst/`. That's the whole point of the provider being swappable at
//! the config level (Phase 3's design); importing a concrete provider
//! crate in this file would break the swap.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::analyst::embedder::Embedder;
use crate::storage::{HostRegistry, MetricHistory};
use crate::trigger::Problem;

/// Returns the four tools the analyst agent loop offers the model.
pub fn tool_definitions() -> Vec<nest_ai::ToolDefinition> {
    vec![
        nest_ai::ToolDefinition::new(
            "get_host_status",
            "Returns online/offline status and last-seen time for a host.",
            serde_json::json!({
                "type": "object",
                "properties": { "host_id": { "type": "string" } },
                "required": ["host_id"]
            }),
        ),
        nest_ai::ToolDefinition::new(
            "get_metric_history",
            "Returns recent values for a specific metric key on a host.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "host_id": { "type": "string" },
                    "key": { "type": "string" },
                    "minutes": { "type": "integer", "description": "how far back to look" }
                },
                "required": ["host_id", "key"]
            }),
        ),
        nest_ai::ToolDefinition::new(
            "get_active_problems",
            "Returns currently open Problems, optionally filtered by host.",
            serde_json::json!({
                "type": "object",
                "properties": { "host_id": { "type": "string" } }
            }),
        ),
        nest_ai::ToolDefinition::new(
            "search_similar_incidents",
            "Finds past resolved Problems with a similar description, and how they were resolved.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["description"]
            }),
        ),
    ]
}

/// Executes one tool call against Sparrow's own data, returning the result
/// as a JSON string — the content of the `ChatMessage::tool_result` sent
/// back to the model.
///
/// Returns `String` unconditionally, never `Result` — deliberate. A failed
/// tool call (bad arguments, a DB error, an unknown tool name) is normal
/// conversational flow: the model sees `{"error": "..."}` in the tool
/// result and can retry with corrected arguments or explain the failure to
/// the user, rather than the whole agent loop aborting on what's often
/// just the model guessing a slightly wrong argument shape — especially
/// likely on a weaker local provider.
pub async fn execute_tool(
    call: &nest_ai::ToolCall,
    pool: &PgPool,
    embedder: &dyn Embedder,
) -> String {
    let result = match call.name.as_str() {
        "get_host_status" => get_host_status(pool, &call.arguments).await,
        "get_metric_history" => get_metric_history(pool, &call.arguments).await,
        "get_active_problems" => get_active_problems(pool, &call.arguments).await,
        "search_similar_incidents" => {
            search_similar_incidents(pool, embedder, &call.arguments).await
        }
        other => Err(format!("unknown tool: {other}")),
    };
    match result {
        Ok(value) => value.to_string(),
        Err(err) => serde_json::json!({ "error": err }).to_string(),
    }
}

#[derive(Deserialize)]
struct GetHostStatusArgs {
    host_id: String,
}

async fn get_host_status(
    pool: &PgPool,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let args: GetHostStatusArgs = serde_json::from_value(args.clone())
        .map_err(|error| format!("invalid arguments for get_host_status: {error}"))?;

    let host = HostRegistry::new(pool.clone())
        .get(&args.host_id)
        .await
        .map_err(|error| error.to_string())?;

    match host {
        Some(host) => serde_json::to_value(host).map_err(|error| error.to_string()),
        None => Err(format!(
            "no host registered with host_id {:?}",
            args.host_id
        )),
    }
}

#[derive(Deserialize)]
struct GetMetricHistoryArgs {
    host_id: String,
    key: String,
    #[serde(default)]
    minutes: Option<i64>,
}

/// Cap on how many `metric_history` rows a single tool call returns —
/// keeps the tool result a reasonable size regardless of how far back
/// `minutes` asks to look, independent of `api/history.rs`'s own
/// (unrelated, HTTP-client-facing) `MAX_LIMIT`.
const METRIC_HISTORY_TOOL_LIMIT: i64 = 200;
/// Default lookback when `minutes` is omitted.
const DEFAULT_METRIC_HISTORY_MINUTES: i64 = 60;

async fn get_metric_history(
    pool: &PgPool,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let args: GetMetricHistoryArgs = serde_json::from_value(args.clone())
        .map_err(|error| format!("invalid arguments for get_metric_history: {error}"))?;
    let minutes = args
        .minutes
        .unwrap_or(DEFAULT_METRIC_HISTORY_MINUTES)
        .max(1);
    let from_ms = crate::time::now_ms() - minutes * 60_000;

    let rows = MetricHistory::new(pool.clone())
        .history(
            &args.host_id,
            &args.key,
            Some(from_ms),
            None,
            METRIC_HISTORY_TOOL_LIMIT,
        )
        .await
        .map_err(|error| error.to_string())?;

    serde_json::to_value(rows).map_err(|error| error.to_string())
}

#[derive(Deserialize, Default)]
struct GetActiveProblemsArgs {
    #[serde(default)]
    host_id: Option<String>,
}

async fn get_active_problems(
    pool: &PgPool,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Unlike the other three tools, every argument here is optional, so a
    // model that omits `arguments` entirely (ToolCall::arguments defaults
    // to Value::Null in that case) is a valid call, not a malformed one —
    // serde_json::from_value(Value::Null, ..) fails for a struct target
    // regardless of per-field #[serde(default)], so that case is handled
    // explicitly rather than surfacing as an error.
    let args: GetActiveProblemsArgs = if args.is_null() {
        GetActiveProblemsArgs::default()
    } else {
        serde_json::from_value(args.clone())
            .map_err(|error| format!("invalid arguments for get_active_problems: {error}"))?
    };

    let problems: Vec<Problem> = match &args.host_id {
        Some(host_id) => sqlx::query_as::<_, Problem>(
            "SELECT id, rule_id, host_id, status, opened_at, resolved_at, last_value
             FROM problems
             WHERE status = 'open' AND host_id = $1
             ORDER BY opened_at DESC",
        )
        .bind(host_id)
        .fetch_all(pool)
        .await
        .map_err(|error| error.to_string())?,
        None => sqlx::query_as::<_, Problem>(
            "SELECT id, rule_id, host_id, status, opened_at, resolved_at, last_value
             FROM problems
             WHERE status = 'open'
             ORDER BY opened_at DESC",
        )
        .fetch_all(pool)
        .await
        .map_err(|error| error.to_string())?,
    };

    serde_json::to_value(problems).map_err(|error| error.to_string())
}

#[derive(Deserialize)]
struct SearchSimilarIncidentsArgs {
    description: String,
    #[serde(default)]
    limit: Option<i64>,
}

/// Row shape returned to the model. `nest_data_postgres::SimilarityHit`
/// doesn't derive `Serialize` (it's a framework type outside this crate,
/// so the orphan rule rules out adding the impl here) — this is a local
/// mirror, not a duplicate abstraction.
#[derive(Serialize)]
struct SimilarIncident {
    id: String,
    distance: f32,
}

/// Uses Issue 10.1's [`Embedder`] and Issue 10.4's `resolved_incidents`
/// table (`crate::migrations`'s `007_create_resolved_incidents`,
/// populated when a `Problem` resolves — see
/// `crates/server/src/alerting.rs`'s `resolve_problem`).
async fn search_similar_incidents(
    pool: &PgPool,
    embedder: &dyn Embedder,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let args: SearchSimilarIncidentsArgs = serde_json::from_value(args.clone())
        .map_err(|error| format!("invalid arguments for search_similar_incidents: {error}"))?;
    let limit = args.limit.unwrap_or(5).clamp(1, 50) as u32;

    let embedding = embedder
        .embed(&args.description)
        .await
        .map_err(|error| error.to_string())?;

    let search =
        nest_data_postgres::VectorSearch::new(pool, "resolved_incidents", "id", "embedding");
    let hits = search
        .search_similar(&embedding, limit, None)
        .await
        .map_err(|error| error.to_string())?;

    let hits: Vec<SimilarIncident> = hits
        .into_iter()
        .map(|hit| SimilarIncident {
            id: hit.id,
            distance: hit.distance,
        })
        .collect();

    serde_json::to_value(hits).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use nest_data::DataModule;
    use nest_data_postgres::{PostgresConfig, PostgresDataModule};
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt};

    use super::*;
    use crate::analyst::embedder::EMBEDDING_DIMENSION;
    use crate::storage::HostRegistry;

    /// Holds a running Postgres container (with Sparrow's migrations
    /// already applied) alive for the test's duration. Same recipe as
    /// `storage.rs`/`migrations.rs`/`alerting.rs`'s own test modules
    /// (duplicated, not imported — private to each module's tests).
    /// `pgvector/pgvector:pg16`, not the plain `postgres` image — Issue
    /// 10.4's `resolved_incidents` migration needs the `vector` extension
    /// installable.
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
                    .with_migrations(crate::migrations::all_migrations()),
            )
            .build()
            .expect("app with postgres migrations");

        let pool = PgPool::connect(&url).await.expect("fresh postgres pool");
        TestDb {
            _container: container,
            pool,
        }
    }

    /// Never used for a real embedding in these tests — `search_similar_incidents`
    /// tests below only exercise the error path (Issue 10.4's
    /// `resolved_incidents` table doesn't exist yet), so this just needs to
    /// return *something* without needing a real Ollama instance.
    struct FakeEmbedder;

    #[async_trait::async_trait]
    impl Embedder for FakeEmbedder {
        async fn embed(&self, _text: &str) -> nest_error::NestResult<Vec<f32>> {
            Ok(vec![0.0; crate::analyst::embedder::EMBEDDING_DIMENSION])
        }
    }

    #[test]
    fn tool_definitions_returns_the_four_expected_tools() {
        let definitions = tool_definitions();
        let names: Vec<&str> = definitions.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "get_host_status",
                "get_metric_history",
                "get_active_problems",
                "search_similar_incidents",
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_tool_reports_an_unknown_tool_name_as_an_error_string_not_a_panic() {
        let db = start_postgres_with_schema().await;
        let call = nest_ai::ToolCall::new("not_a_real_tool", serde_json::json!({}));

        let result = execute_tool(&call, &db.pool, &FakeEmbedder).await;

        let value: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert_eq!(value["error"], "unknown tool: not_a_real_tool");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_host_status_returns_the_registered_hosts_row() {
        let db = start_postgres_with_schema().await;
        HostRegistry::new(db.pool.clone())
            .upsert_on_register("tools-host-status", "tools-test-host")
            .await
            .expect("seed host");

        let call = nest_ai::ToolCall::new(
            "get_host_status",
            serde_json::json!({ "host_id": "tools-host-status" }),
        );
        let result = execute_tool(&call, &db.pool, &FakeEmbedder).await;

        let value: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert_eq!(value["host_id"], "tools-host-status");
        assert_eq!(value["hostname"], "tools-test-host");
        assert_eq!(value["online"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_host_status_reports_an_unregistered_host_as_an_error_string() {
        let db = start_postgres_with_schema().await;

        let call = nest_ai::ToolCall::new(
            "get_host_status",
            serde_json::json!({ "host_id": "never-registered" }),
        );
        let result = execute_tool(&call, &db.pool, &FakeEmbedder).await;

        let value: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert!(value["error"]
            .as_str()
            .expect("error should be a string")
            .contains("never-registered"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_host_status_reports_malformed_arguments_as_an_error_string_not_a_panic() {
        let db = start_postgres_with_schema().await;
        // Missing the required `host_id` field entirely.
        let call = nest_ai::ToolCall::new("get_host_status", serde_json::json!({}));

        let result = execute_tool(&call, &db.pool, &FakeEmbedder).await;

        let value: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert!(value["error"].is_string());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_metric_history_returns_rows_within_the_requested_window() {
        let db = start_postgres_with_schema().await;
        let host_id = "tools-metric-history";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "tools-test-host")
            .await
            .expect("seed host");

        let history = MetricHistory::new(db.pool.clone());
        let now = crate::time::now_ms();
        for (offset_minutes, value) in [(120, "10"), (30, "20"), (1, "30")] {
            history
                .insert_batch(
                    host_id,
                    &crate::transport::DataBatch {
                        host_id: host_id.to_string(),
                        collector: "cpu".to_string(),
                        items: vec![crate::collector::MetricItem {
                            key: "cpu.usage_percent".to_string(),
                            value_type: crate::collector::ValueType::Float,
                            value: value.to_string(),
                            tags: Default::default(),
                            timestamp_ms: now - offset_minutes * 60_000,
                        }],
                    },
                )
                .await
                .expect("insert metric point");
        }

        let call = nest_ai::ToolCall::new(
            "get_metric_history",
            serde_json::json!({ "host_id": host_id, "key": "cpu.usage_percent", "minutes": 60 }),
        );
        let result = execute_tool(&call, &db.pool, &FakeEmbedder).await;

        let value: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        let rows = value.as_array().expect("rows should be an array");
        // Only the 30-minutes-ago and 1-minute-ago points fall inside a
        // 60-minute window; the 120-minutes-ago point must be excluded.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["value"], "30");
        assert_eq!(rows[1]["value"], "20");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_active_problems_filters_by_host_when_given_and_lists_all_when_omitted() {
        let db = start_postgres_with_schema().await;
        let host_a = "tools-problems-host-a";
        let host_b = "tools-problems-host-b";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_a, "host-a")
            .await
            .expect("seed host a");
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_b, "host-b")
            .await
            .expect("seed host b");

        for host_id in [host_a, host_b] {
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
        }

        let call = nest_ai::ToolCall::new(
            "get_active_problems",
            serde_json::json!({ "host_id": host_a }),
        );
        let result = execute_tool(&call, &db.pool, &FakeEmbedder).await;
        let value: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        let rows = value.as_array().expect("rows should be an array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["host_id"], host_a);

        // No arguments at all — ToolCall::arguments defaults to Value::Null
        // in that case, which must list every open problem, not error.
        let call_all = nest_ai::ToolCall::new("get_active_problems", serde_json::Value::Null);
        let result_all = execute_tool(&call_all, &db.pool, &FakeEmbedder).await;
        let value_all: serde_json::Value = serde_json::from_str(&result_all).expect("valid JSON");
        let rows_all = value_all.as_array().expect("rows should be an array");
        assert_eq!(rows_all.len(), 2);
    }

    /// Returns a fixed, caller-supplied vector regardless of input text —
    /// unlike `FakeEmbedder` (always all-zeros, fine for tests that never
    /// inspect the embedding), this lets a test control exactly which
    /// seeded `resolved_incidents` row should come back closest.
    struct FixedEmbedder(Vec<f32>);

    #[async_trait::async_trait]
    impl Embedder for FixedEmbedder {
        async fn embed(&self, _text: &str) -> nest_error::NestResult<Vec<f32>> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_similar_incidents_returns_the_closest_seeded_incident_first() {
        let db = start_postgres_with_schema().await;
        let host_id = "tools-similar-incidents";
        HostRegistry::new(db.pool.clone())
            .upsert_on_register(host_id, "tools-test-host")
            .await
            .expect("seed host");

        let mut near = vec![0.0_f32; EMBEDDING_DIMENSION];
        near[0] = 1.0;
        let mut far = vec![0.0_f32; EMBEDDING_DIMENSION];
        far[0] = -1.0;

        let near_id: i64 = sqlx::query_scalar(
            "INSERT INTO resolved_incidents (host_id, problem_description, resolution_notes, embedding)
             VALUES ($1, 'disk usage exceeded 90 percent on host web-01', 'auto-resolved', $2)
             RETURNING id",
        )
        .bind(host_id)
        .bind(pgvector::Vector::from(near.clone()))
        .fetch_one(&db.pool)
        .await
        .expect("seed near resolved_incidents row");

        sqlx::query(
            "INSERT INTO resolved_incidents (host_id, problem_description, resolution_notes, embedding)
             VALUES ($1, 'completely unrelated incident about memory', 'auto-resolved', $2)",
        )
        .bind(host_id)
        .bind(pgvector::Vector::from(far))
        .execute(&db.pool)
        .await
        .expect("seed far resolved_incidents row");

        let call = nest_ai::ToolCall::new(
            "search_similar_incidents",
            serde_json::json!({ "description": "disk usage exceeded 90 percent", "limit": 1 }),
        );
        let result = execute_tool(&call, &db.pool, &FixedEmbedder(near)).await;

        let value: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        let rows = value.as_array().expect("rows should be an array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], near_id.to_string());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_similar_incidents_reports_missing_description_as_an_error_string() {
        let db = start_postgres_with_schema().await;
        let call = nest_ai::ToolCall::new("search_similar_incidents", serde_json::json!({}));

        let result = execute_tool(&call, &db.pool, &FakeEmbedder).await;

        let value: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert!(value["error"].is_string());
    }
}
