//! `GET /hosts` and `GET /hosts/:id/items`.
//!
//! `RequestContext` (verified against `nest-http-serve/src/context.rs`
//! directly, not guessed) has no way to reach registered `AppContext`
//! services — it only carries the method/path/query/headers/params/body of
//! the current request. `RouteGroup::get`'s handlers are plain
//! `Fn(RequestContext) -> Fut` closures, so shared state (here,
//! `HostRegistry`/`MetricHistory`) has to be captured by the closure
//! `routes()` builds, not fetched from inside the handler through some
//! context accessor — hence `routes()` taking them as parameters and each
//! route closure cloning its own copy per call (`Fn`, not `FnOnce`, is
//! called once per incoming request).

use nest_error::NestError;
use nest_http_serve::{HttpResult, Json, RequestContext, RouteGroup, ServeError};
use sparrow_core::storage::{HostRegistry, MetricHistory};

/// Builds the `/api/hosts` and `/api/hosts/:id/items` routes.
pub fn routes(registry: HostRegistry, history: MetricHistory) -> RouteGroup {
    RouteGroup::new("/api")
        .get("/hosts", move |ctx| {
            let registry = registry.clone();
            async move { list_hosts(ctx, registry).await }
        })
        .get("/hosts/:id/items", move |ctx| {
            let history = history.clone();
            async move { get_latest_items(ctx, history).await }
        })
}

async fn list_hosts(_ctx: RequestContext, registry: HostRegistry) -> HttpResult {
    let hosts = registry
        .list()
        .await
        .map_err(|error| ServeError::from(NestError::unknown(error.to_string())))?;
    Json(hosts).into_response()
}

async fn get_latest_items(ctx: RequestContext, history: MetricHistory) -> HttpResult {
    let host_id = ctx.param("id")?;
    let items = history
        .latest_items(host_id)
        .await
        .map_err(|error| ServeError::from(NestError::unknown(error.to_string())))?;
    Json(items).into_response()
}

#[cfg(test)]
mod tests {
    use nest_http_serve::{HttpMethod, RouteRegistry};
    use sqlx::PgPool;

    use super::*;

    /// Confirms `routes()` wires up the two expected patterns/methods.
    /// Doesn't need a live Postgres — `PgPool::connect_lazy` never opens a
    /// connection, and building a `RouteGroup` only needs owned
    /// `HostRegistry`/`MetricHistory` values to move into the closures, not
    /// a working pool (it still needs *a* Tokio context to construct,
    /// though — same reason sparrow-core's own `connect_lazy` test is
    /// `#[tokio::test]`, not a plain `#[test]`). The handlers' actual
    /// DB-backed behavior needs a real HTTP+Postgres integration test,
    /// deferred to Issue 7.5 like every other Docker-dependent test in this
    /// milestone.
    #[tokio::test]
    async fn routes_registers_the_expected_patterns_and_methods() {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");

        let mut registry = RouteRegistry::new();
        registry.add_group(routes(
            HostRegistry::new(pool.clone()),
            MetricHistory::new(pool),
        ));

        let mut found: Vec<(HttpMethod, &str)> = registry
            .routes()
            .iter()
            .map(|route| (route.method, route.pattern.as_str()))
            .collect();
        found.sort_by_key(|(_, pattern)| *pattern);

        assert_eq!(
            found,
            vec![
                (HttpMethod::Get, "/api/hosts"),
                (HttpMethod::Get, "/api/hosts/:id/items"),
            ]
        );
    }
}
