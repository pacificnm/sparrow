//! `GET /hosts/:id/items/:key/history`.
//!
//! Same `RequestContext` ground truth as `api/hosts.rs` (verified against
//! `nest-http-serve/src/context.rs` directly): `ctx.query(name) ->
//! Option<&str>` for optional query params, no service-access method exists
//! at all, so `MetricHistory` is captured by the route closure `routes()`
//! builds rather than fetched from inside the handler.

use nest_error::NestError;
use nest_http_serve::{HttpResult, Json, RequestContext, RouteGroup, ServeError};
use sparrow_core::storage::MetricHistory;

/// Default row cap when the client doesn't specify `limit`.
const DEFAULT_LIMIT: i64 = 1000;
/// Hard cap on `limit` regardless of what the client requests — this
/// endpoint reads `metric_history` directly, and a chatty client asking for
/// an unbounded row set must never be able to pull one.
const MAX_LIMIT: i64 = 10_000;

/// Builds the `/api/hosts/:id/items/:key/history` route.
pub fn routes(history: MetricHistory) -> RouteGroup {
    RouteGroup::new("/api").get("/hosts/:id/items/:key/history", move |ctx| {
        let history = history.clone();
        async move { get_history(ctx, history).await }
    })
}

async fn get_history(ctx: RequestContext, history: MetricHistory) -> HttpResult {
    let host_id = ctx.param("id")?;
    let key = ctx.param("key")?;

    let from_ms = parse_optional_i64_query(&ctx, "from_ms")?;
    let to_ms = parse_optional_i64_query(&ctx, "to_ms")?;
    let limit = resolve_limit(ctx.query("limit"))?;

    let rows = history
        .history(host_id, key, from_ms, to_ms, limit)
        .await
        .map_err(|error| ServeError::from(NestError::unknown(error.to_string())))?;

    Json(rows).into_response()
}

/// Resolves the `limit` query param: absent → [`DEFAULT_LIMIT`], present →
/// parsed and clamped into `1..=MAX_LIMIT` (a client asking for more than
/// the hard max gets the max, not an error — only a non-numeric value is
/// rejected).
fn resolve_limit(raw: Option<&str>) -> Result<i64, ServeError> {
    match raw {
        Some(raw) => raw
            .parse::<i64>()
            .map(|limit| limit.clamp(1, MAX_LIMIT))
            .map_err(|_| invalid_query_error("limit", raw)),
        None => Ok(DEFAULT_LIMIT),
    }
}

/// Parses an optional `i64` query parameter, distinguishing "absent"
/// (`Ok(None)`) from "present but not a valid integer" (an error, not
/// silently treated as absent — a malformed `from_ms`/`to_ms` should tell
/// the client it was ignored-by-mistake, not just get dropped).
fn parse_optional_i64_query(ctx: &RequestContext, name: &str) -> Result<Option<i64>, ServeError> {
    match ctx.query(name) {
        Some(raw) => raw
            .parse::<i64>()
            .map(Some)
            .map_err(|_| invalid_query_error(name, raw)),
        None => Ok(None),
    }
}

fn invalid_query_error(name: &str, raw: &str) -> ServeError {
    ServeError::from(NestError::validation(format!(
        "invalid {name}: {raw:?} is not an integer"
    )))
}

#[cfg(test)]
mod tests {
    use nest_http_serve::{HttpMethod, RouteRegistry};
    use sqlx::PgPool;

    use super::*;

    /// Confirms `routes()` wires up the expected pattern/method, the same
    /// non-Docker check `api/hosts.rs`'s own test does. The handler's
    /// DB-backed behavior (range/limit clamping against real rows) needs a
    /// real HTTP+Postgres integration test, deferred to Issue 7.5.
    #[tokio::test]
    async fn routes_registers_the_expected_pattern_and_method() {
        let pool = PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection");

        let mut registry = RouteRegistry::new();
        registry.add_group(routes(MetricHistory::new(pool)));

        let found: Vec<(HttpMethod, &str)> = registry
            .routes()
            .iter()
            .map(|route| (route.method, route.pattern.as_str()))
            .collect();

        assert_eq!(
            found,
            vec![(HttpMethod::Get, "/api/hosts/:id/items/:key/history")]
        );
    }

    #[test]
    fn resolve_limit_defaults_when_absent() {
        assert_eq!(resolve_limit(None).unwrap(), DEFAULT_LIMIT);
    }

    #[test]
    fn resolve_limit_clamps_above_the_hard_max() {
        assert_eq!(resolve_limit(Some("50000")).unwrap(), MAX_LIMIT);
    }

    #[test]
    fn resolve_limit_clamps_zero_and_negative_up_to_one() {
        assert_eq!(resolve_limit(Some("0")).unwrap(), 1);
        assert_eq!(resolve_limit(Some("-5")).unwrap(), 1);
    }

    #[test]
    fn resolve_limit_passes_through_values_within_range() {
        assert_eq!(resolve_limit(Some("500")).unwrap(), 500);
    }

    #[test]
    fn resolve_limit_rejects_non_numeric_values() {
        assert!(resolve_limit(Some("not-a-number")).is_err());
    }
}
