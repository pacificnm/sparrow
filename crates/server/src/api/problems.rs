//! `GET /api/problems`.
//!
//! Same `RequestContext` ground truth as `api/hosts.rs`/`api/history.rs`
//! (verified against `nest-http-serve/src/context.rs` directly):
//! `ctx.query(name) -> Option<&str>` for the optional `host_id` filter, no
//! service-access method exists, so `PgPool` is captured by the route
//! closure `routes()` builds.

use nest_error::NestError;
use nest_http_serve::{HttpResult, Json, RequestContext, RouteGroup, ServeError};
use sparrow_core::trigger::Problem;
use sqlx::PgPool;

/// Builds the `/api/problems` route.
pub fn routes(pool: PgPool) -> RouteGroup {
    RouteGroup::new("/api").get("/problems", move |ctx| {
        let pool = pool.clone();
        async move { list_problems(ctx, pool).await }
    })
}

async fn list_problems(ctx: RequestContext, pool: PgPool) -> HttpResult {
    let host_id = ctx.query("host_id");
    let problems = fetch_open_problems(&pool, host_id)
        .await
        .map_err(|error| ServeError::from(NestError::unknown(error.to_string())))?;
    Json(problems).into_response()
}

/// Returns every currently `Open` problem, optionally filtered by
/// `host_id` — omitted means "all currently open problems across all
/// hosts," per the phase-8 spec's own instruction.
///
/// `pub(crate)`, not private: `api/analyst.rs`'s "explain this Problem"
/// prompt synthesis (Issue 11.2) reuses this exact query rather than
/// duplicating it.
pub(crate) async fn fetch_open_problems(
    pool: &PgPool,
    host_id: Option<&str>,
) -> sqlx::Result<Vec<Problem>> {
    match host_id {
        Some(host_id) => {
            sqlx::query_as::<_, Problem>(
                "SELECT id, rule_id, host_id, status, opened_at, resolved_at, last_value
                 FROM problems
                 WHERE status = 'open' AND host_id = $1
                 ORDER BY opened_at DESC",
            )
            .bind(host_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, Problem>(
                "SELECT id, rule_id, host_id, status, opened_at, resolved_at, last_value
                 FROM problems
                 WHERE status = 'open'
                 ORDER BY opened_at DESC",
            )
            .fetch_all(pool)
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use nest_http_serve::{HttpMethod, RouteRegistry};

    use super::*;

    #[tokio::test]
    async fn routes_registers_the_expected_pattern_and_method() {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");

        let mut registry = RouteRegistry::new();
        registry.add_group(routes(pool));

        let found: Vec<(HttpMethod, &str)> = registry
            .routes()
            .iter()
            .map(|route| (route.method, route.pattern.as_str()))
            .collect();

        assert_eq!(found, vec![(HttpMethod::Get, "/api/problems")]);
    }

    // --- Docker-backed tests below: real seeded Postgres data, per this
    // issue's acceptance ("both endpoints return real data against a
    // seeded server").

    use nest_data::DataModule;
    use nest_data_postgres::{PostgresConfig, PostgresDataModule};
    use sparrow_core::storage::HostRegistry;
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt};

    /// Holds a running Postgres container (with Sparrow's migrations
    /// already applied) alive for the test's duration. Same recipe as
    /// `alerting.rs`'s/`api/agent_config.rs`'s own test modules
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

    async fn seed_open_problem(pool: &PgPool, host_id: &str) {
        HostRegistry::new(pool.clone())
            .upsert_on_register(host_id, "test-host")
            .await
            .expect("seed host for the FK reference");

        let rule_id: i64 = sqlx::query_scalar(
            "INSERT INTO rules (host_id, item_key, operator, threshold, severity)
             VALUES ($1, 'cpu.usage_percent', 'greater_than', 90.0, 'warning')
             RETURNING id",
        )
        .bind(host_id)
        .fetch_one(pool)
        .await
        .expect("insert rule");

        sqlx::query(
            "INSERT INTO problems (rule_id, host_id, status, opened_at, last_value)
             VALUES ($1, $2, 'open', 0, 95.0)",
        )
        .bind(rule_id)
        .bind(host_id)
        .execute(pool)
        .await
        .expect("insert open problem");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_open_problems_filters_by_host_when_given_and_lists_all_when_omitted() {
        let db = start_postgres_with_schema().await;
        seed_open_problem(&db.pool, "problems-fetch-host-a").await;
        seed_open_problem(&db.pool, "problems-fetch-host-b").await;

        let filtered = fetch_open_problems(&db.pool, Some("problems-fetch-host-a"))
            .await
            .expect("fetch_open_problems should succeed");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].host_id, "problems-fetch-host-a");

        let all = fetch_open_problems(&db.pool, None)
            .await
            .expect("fetch_open_problems should succeed");
        assert_eq!(all.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_problems_http_endpoint_returns_seeded_problems_filtered_by_host_id() {
        use nest_http_serve::HttpServer;

        let db = start_postgres_with_schema().await;
        seed_open_problem(&db.pool, "problems-http-host-a").await;
        seed_open_problem(&db.pool, "problems-http-host-b").await;

        let server = HttpServer::builder()
            .routes(routes(db.pool.clone()))
            .spawn()
            .await
            .expect("http server should spawn");

        let http_client = reqwest::Client::new();
        let response = http_client
            .get(format!(
                "{}/api/problems?host_id=problems-http-host-a",
                server.base_url()
            ))
            .send()
            .await
            .expect("GET /api/problems should succeed");
        assert!(response.status().is_success());

        let body: serde_json::Value = response.json().await.expect("valid JSON body");
        let problems = body.as_array().expect("response should be an array");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0]["host_id"], "problems-http-host-a");

        server.shutdown().await;
    }
}
