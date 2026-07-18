//! `run_analysis` Tauri command.
//!
//! Calls `POST /api/analyst/run`, specified in Phase 10's own spec
//! (`docs/plans/phase-10-ai-health-analyst.md`, "API" section) and
//! implemented server-side in Issue 11.2. `RunAnalysisRequest` mirrors that
//! spec's own sketch exactly (`host_id`, `question`, `mode`) — not a shape
//! invented independently here.

use nest_error::NestResult;
use serde::{Deserialize, Serialize};
use sparrow_core::analyst::r#loop::AnalysisMode;

use crate::state::AppState;

#[derive(Serialize)]
struct RunAnalysisRequest {
    host_id: Option<String>,
    question: Option<String>,
    mode: AnalysisMode,
}

/// The response shape this client assumes `POST /api/analyst/run` returns.
/// Phase 10's spec says only "return the response text as JSON" — this is
/// the joint contract Issue 11.2's server handler must implement exactly
/// (`{"response": "..."}`, not a bare JSON string), so the two sides don't
/// silently diverge.
#[derive(Debug, Deserialize)]
struct RunAnalysisResponse {
    response: String,
}

#[tauri::command]
pub async fn run_analysis(
    state: tauri::State<'_, AppState>,
    host_id: Option<String>,
    question: Option<String>,
    mode: AnalysisMode,
) -> NestResult<String> {
    run_analysis_via(&state, host_id, question, mode).await
}

async fn run_analysis_via(
    state: &AppState,
    host_id: Option<String>,
    question: Option<String>,
    mode: AnalysisMode,
) -> NestResult<String> {
    let request = RunAnalysisRequest {
        host_id,
        question,
        mode,
    };
    let response: RunAnalysisResponse = state
        .http()
        .post_json(&state.url("/api/analyst/run"), &request)
        .await?;
    Ok(response.response)
}

#[cfg(test)]
mod tests {
    use nest_http_client::{HttpClientConfig, HttpClientService};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn state_for(server: &MockServer) -> AppState {
        let http = HttpClientService::new(HttpClientConfig::default()).expect("http client");
        AppState::new(http, server.uri())
    }

    #[tokio::test]
    async fn run_analysis_via_posts_the_request_and_returns_the_response_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/analyst/run"))
            .and(body_json(serde_json::json!({
                "host_id": "host-1",
                "question": null,
                "mode": "quick"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "response": "all clear" })),
            )
            .mount(&server)
            .await;

        let result = run_analysis_via(
            &state_for(&server),
            Some("host-1".to_string()),
            None,
            AnalysisMode::Quick,
        )
        .await
        .expect("run_analysis_via should succeed");

        assert_eq!(result, "all clear");
    }

    #[tokio::test]
    async fn run_analysis_via_serializes_report_mode_as_snake_case() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/analyst/run"))
            .and(body_json(serde_json::json!({
                "host_id": null,
                "question": "how is the fleet doing?",
                "mode": "report"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "response": "fleet is healthy" })),
            )
            .mount(&server)
            .await;

        let result = run_analysis_via(
            &state_for(&server),
            None,
            Some("how is the fleet doing?".to_string()),
            AnalysisMode::Report,
        )
        .await
        .expect("run_analysis_via should succeed");

        assert_eq!(result, "fleet is healthy");
    }
}
