//! `get_active_problems` Tauri command.
//!
//! Calls `GET /api/problems` (optionally `?host_id=`), specified in Phase
//! 8's own spec (`docs/plans/phase-8-trigger-alerting.md`, "API" section)
//! and implemented server-side in Issue 11.2 — the request shape here must
//! match that contract exactly, not one invented independently.

use nest_error::NestResult;
use serde::{Deserialize, Serialize};
use sparrow_core::trigger::{ProblemStatus, Severity};

use crate::state::AppState;

/// Mirrors `crates/server/src/api/problems.rs`'s own `OpenProblem` wire
/// shape exactly: a `problems` row joined with its owning rule's
/// `severity`. `sparrow_core::trigger::Problem` itself has no `severity`
/// field (that lives on `Rule`) — deserializing the server's response
/// straight into `Problem` silently drops it (serde ignores unknown
/// fields by default), which is exactly what this command used to do.
/// `desktop/ui/src/lib/api.ts`'s `Problem` type has always declared
/// `severity` as required, and `ProblemsPanel.tsx` (Issue 11.3) has always
/// keyed its Chip color off of it — with the old `Problem`-typed return,
/// every Problem the desktop app displayed had `severity: undefined` at
/// runtime, breaking the "severity-colored" requirement silently (no
/// error, just the wrong/missing chip color) since JS doesn't enforce
/// TypeScript's "required" at the IPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenProblem {
    pub id: i64,
    pub rule_id: i64,
    pub host_id: String,
    pub status: ProblemStatus,
    pub opened_at: i64,
    pub resolved_at: Option<i64>,
    pub last_value: f64,
    pub severity: Severity,
}

#[tauri::command]
pub async fn get_active_problems(
    state: tauri::State<'_, AppState>,
    host_id: Option<String>,
) -> NestResult<Vec<OpenProblem>> {
    get_active_problems_via(&state, host_id.as_deref()).await
}

async fn get_active_problems_via(
    state: &AppState,
    host_id: Option<&str>,
) -> NestResult<Vec<OpenProblem>> {
    let path = match host_id {
        Some(host_id) => format!("/api/problems?host_id={host_id}"),
        None => "/api/problems".to_string(),
    };
    state.http().get_json(&state.url(&path)).await
}

#[cfg(test)]
mod tests {
    use nest_http_client::{HttpClientConfig, HttpClientService};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn state_for(server: &MockServer) -> AppState {
        let http = HttpClientService::new(HttpClientConfig::default()).expect("http client");
        AppState::new(http, server.uri())
    }

    fn sample_problem_json() -> serde_json::Value {
        serde_json::json!({
            "id": 1,
            "rule_id": 1,
            "host_id": "host-1",
            "status": "open",
            "opened_at": 1700000000000_i64,
            "resolved_at": null,
            "last_value": 95.0,
            "severity": "critical"
        })
    }

    #[tokio::test]
    async fn get_active_problems_via_omits_the_query_param_when_host_id_is_absent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/problems"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([sample_problem_json()])),
            )
            .mount(&server)
            .await;

        let problems = get_active_problems_via(&state_for(&server), None)
            .await
            .expect("get_active_problems_via should succeed");

        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].host_id, "host-1");
    }

    #[tokio::test]
    async fn get_active_problems_via_includes_the_host_id_query_param_when_given() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/problems"))
            .and(query_param("host_id", "host-1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([sample_problem_json()])),
            )
            .mount(&server)
            .await;

        let problems = get_active_problems_via(&state_for(&server), Some("host-1"))
            .await
            .expect("get_active_problems_via should succeed");

        assert_eq!(problems.len(), 1);
    }

    #[tokio::test]
    async fn get_active_problems_via_preserves_severity_from_the_joined_rule() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/problems"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([sample_problem_json()])),
            )
            .mount(&server)
            .await;

        let problems = get_active_problems_via(&state_for(&server), None)
            .await
            .expect("get_active_problems_via should succeed");

        assert_eq!(
            problems[0].severity,
            Severity::Critical,
            "severity must survive the round trip — ProblemsPanel.tsx's chip \
             color depends on it being present, not silently dropped like \
             sparrow_core::trigger::Problem (no severity field) would do"
        );
    }
}
