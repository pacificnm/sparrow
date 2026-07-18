//! `get_active_problems` Tauri command.
//!
//! Calls `GET /api/problems` (optionally `?host_id=`), specified in Phase
//! 8's own spec (`docs/plans/phase-8-trigger-alerting.md`, "API" section)
//! and implemented server-side in Issue 11.2 — the request shape here must
//! match that contract exactly, not one invented independently.

use nest_error::NestResult;
use sparrow_core::trigger::Problem;

use crate::state::AppState;

#[tauri::command]
pub async fn get_active_problems(
    state: tauri::State<'_, AppState>,
    host_id: Option<String>,
) -> NestResult<Vec<Problem>> {
    get_active_problems_via(&state, host_id.as_deref()).await
}

async fn get_active_problems_via(
    state: &AppState,
    host_id: Option<&str>,
) -> NestResult<Vec<Problem>> {
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
            "last_value": 95.0
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
}
