//! Tool-calling agent loop for the AI Health Analyst (Issue 10.3).
//!
//! ## `AiService` accessor — verified against real source
//!
//! `nest_ai::AiService` (`core/crates/nest-ai/src/service.rs`) itself
//! implements `complete()`/`stream_complete()` directly — it is not a thin
//! wrapper requiring a separate `.provider()` accessor to reach the
//! underlying `AiProvider`. [`run_analysis`] below calls `ai.complete(request)`
//! directly, matching the spec's sketch as written.
//!
//! ## `AnalysisMode::Report` — verified against real source, not implemented as speculated
//!
//! `nest_ai::CompletionRequest` (`core/crates/nest-ai/src/types.rs`) still
//! has no generic "effort"/"thinking" field, and `nest-ai-claude`'s
//! `ClaudeAiProvider` (`modules/crates/nest-ai-claude/src/provider.rs`)
//! exposes no extended-thinking-related method or config either — grepped
//! both crates for "effort"/"thinking": zero hits in either. **Neither of
//! the two options this issue names (a generic `CompletionRequest` field,
//! or a `ClaudeAiProvider`-specific escape hatch) currently exists in the
//! framework.** Until one of those lands upstream, [`AnalysisMode::Report`]
//! degrades to identical behavior as [`AnalysisMode::Quick`] for *every*
//! provider, not just `nest-ai-ollama` — there is currently no way to
//! request extended thinking from any provider through `nest_ai`. Ground
//! truth is re-checked on every call (matching on
//! [`nest_ai::AiService::provider_id`], not cached), so this degrades to
//! real behavior automatically the moment either upstream option ships,
//! without needing to revisit this function — a new match arm is all a
//! future capability addition needs, not a redesign.

use nest_ai::{AiService, ChatMessage, CompletionRequest};
use nest_error::{NestError, NestResult};
use sqlx::PgPool;

use crate::analyst::embedder::Embedder;
use crate::analyst::tools;

/// Hard cap on tool-calling rounds within one [`run_analysis`] call — never
/// let a bad prompt (or a model stuck re-calling the same tool) spin
/// forever; exceeding it returns an `Err`, not an infinite loop.
const MAX_TOOL_ROUNDS: usize = 6;

/// The two analysis modes from the original project plan.
///
/// `Serialize`/`Deserialize` (snake_case, matching every other wire enum in
/// this codebase — `Operator`, `Severity`, `ProblemStatus`) so this one type
/// serves both as the internal mode and the wire value `desktop/src-tauri`'s
/// `run_analysis` command (Issue 11.1) sends and `POST /api/analyst/run`
/// (Issue 11.2) receives — no separate `AnalysisModeWire` type, keeping one
/// definition instead of two that could drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisMode {
    /// Fast per-Problem explanation — whichever provider is configured
    /// runs its normal (cheap/fast) path.
    Quick,
    /// Slower periodic health-trend report. See this module's doc comment
    /// for why this currently behaves identically to `Quick` for every
    /// provider — no upstream hook exists yet to actually request extended
    /// thinking.
    Report,
}

/// Runs the tool-calling agent loop: sends `system_prompt`/`user_prompt` to
/// `ai`, executes any requested tools against `pool`/`embedder`, feeds
/// their results back, and repeats until the model responds without
/// further tool calls (or [`MAX_TOOL_ROUNDS`] is hit).
pub async fn run_analysis(
    ai: &AiService,
    pool: &PgPool,
    embedder: &dyn Embedder,
    system_prompt: &str,
    user_prompt: &str,
    thinking_effort: AnalysisMode,
) -> NestResult<String> {
    // See this module's doc comment: neither a generic CompletionRequest
    // field nor a ClaudeAiProvider-specific escape hatch exists yet for
    // extended thinking, so every provider currently runs the same
    // request regardless of `thinking_effort`. Matched (not just ignored)
    // so the degradation is explicit and this is the one place a future
    // capability addition needs to change.
    match (thinking_effort, ai.provider_id()) {
        (AnalysisMode::Quick, _) | (AnalysisMode::Report, _) => {}
    }

    let mut messages = vec![
        ChatMessage::system(system_prompt),
        ChatMessage::user(user_prompt),
    ];

    for _round in 0..MAX_TOOL_ROUNDS {
        let request = CompletionRequest {
            model: None,
            messages: messages.clone(),
            format: None,
            tools: tools::tool_definitions(),
        };
        let response = ai.complete(request).await.map_err(ai_error_to_nest)?;

        if response.tool_calls.is_empty() {
            return Ok(response.content);
        }

        messages.push(ChatMessage::assistant_tool_calls(
            response.tool_calls.clone(),
        ));
        for call in &response.tool_calls {
            let result = tools::execute_tool(call, pool, embedder).await;
            messages.push(ChatMessage::tool_result(&call.name, result));
        }
    }

    Err(NestError::unknown("analysis exceeded max tool-call rounds"))
}

fn ai_error_to_nest(error: nest_ai::AiError) -> NestError {
    NestError::unknown(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use nest_ai::{AiProvider, AiResult, CompletionResponse, ToolCall};
    use sqlx::PgPool;

    use super::*;
    use crate::analyst::embedder::EMBEDDING_DIMENSION;

    /// Never actually invoked by the tests below — they only ever route
    /// through `execute_tool`'s "unknown tool" branch (see
    /// `FakeProvider::TOOL_NAME`), which never touches `Embedder`.
    struct UnusedEmbedder;

    #[async_trait::async_trait]
    impl Embedder for UnusedEmbedder {
        async fn embed(&self, _text: &str) -> NestResult<Vec<f32>> {
            Ok(vec![0.0; EMBEDDING_DIMENSION])
        }
    }

    /// A scripted `AiProvider`: returns a tool call for `tool_rounds`
    /// rounds, then a final plain-content response — or, when
    /// `tool_rounds` is `usize::MAX`, requests a tool call forever (used to
    /// prove `MAX_TOOL_ROUNDS` actually caps the loop). Named
    /// `unknown_test_tool` deliberately — `execute_tool`'s "unknown tool"
    /// branch needs no database, so these tests don't need Docker either;
    /// `tools.rs`'s own dispatch correctness is already covered by issue
    /// #33's tests.
    struct FakeProvider {
        tool_rounds: usize,
        calls: AtomicUsize,
    }

    impl FakeProvider {
        const TOOL_NAME: &'static str = "unknown_test_tool";

        fn new(tool_rounds: usize) -> Self {
            Self {
                tool_rounds,
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl AiProvider for FakeProvider {
        fn provider_id(&self) -> &'static str {
            "fake"
        }

        async fn complete(&self, _request: CompletionRequest) -> AiResult<CompletionResponse> {
            let round = self.calls.fetch_add(1, Ordering::SeqCst);
            if round < self.tool_rounds {
                Ok(CompletionResponse {
                    model: "fake".to_string(),
                    content: String::new(),
                    done: true,
                    tool_calls: vec![ToolCall::new(Self::TOOL_NAME, serde_json::json!({}))],
                    metrics: None,
                })
            } else {
                Ok(CompletionResponse {
                    model: "fake".to_string(),
                    content: "final answer".to_string(),
                    done: true,
                    tool_calls: vec![],
                    metrics: None,
                })
            }
        }
    }

    fn unreachable_pool() -> PgPool {
        PgPool::connect_lazy("postgres://sparrow-tests-unused@127.0.0.1/unused")
            .expect("lazy pool construction should not require a live connection")
    }

    #[tokio::test]
    async fn run_analysis_returns_content_immediately_when_no_tools_are_requested() {
        let provider = Arc::new(FakeProvider::new(0));
        let ai = AiService::new(provider.clone());
        let pool = unreachable_pool();

        let result = run_analysis(
            &ai,
            &pool,
            &UnusedEmbedder,
            "system",
            "user",
            AnalysisMode::Quick,
        )
        .await
        .expect("run_analysis should succeed");

        assert_eq!(result, "final answer");
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "no tool calls means exactly one round"
        );
    }

    #[tokio::test]
    async fn run_analysis_executes_tool_calls_and_continues_until_none_remain() {
        let provider = Arc::new(FakeProvider::new(2));
        let ai = AiService::new(provider.clone());
        let pool = unreachable_pool();

        let result = run_analysis(
            &ai,
            &pool,
            &UnusedEmbedder,
            "system",
            "user",
            AnalysisMode::Quick,
        )
        .await
        .expect("run_analysis should succeed");

        assert_eq!(result, "final answer");
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            3,
            "two tool-call rounds plus the final content round"
        );
    }

    #[tokio::test]
    async fn run_analysis_returns_an_error_after_max_tool_rounds_instead_of_spinning_forever() {
        let provider = Arc::new(FakeProvider::new(usize::MAX));
        let ai = AiService::new(provider.clone());
        let pool = unreachable_pool();

        let error = run_analysis(
            &ai,
            &pool,
            &UnusedEmbedder,
            "system",
            "user",
            AnalysisMode::Quick,
        )
        .await
        .expect_err("a provider that never stops requesting tools must eventually error");

        assert!(error.to_string().contains("max tool-call rounds"));
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            MAX_TOOL_ROUNDS,
            "the hard cap must stop the loop at exactly MAX_TOOL_ROUNDS calls, not spin forever"
        );
    }

    #[tokio::test]
    async fn run_analysis_report_mode_currently_behaves_identically_to_quick() {
        // See this module's doc comment: no upstream hook exists yet to
        // actually differentiate Report from Quick, for any provider —
        // this test pins down that degradation as an observable contract,
        // not just a comment, so a regression here is caught even before
        // a real extended-thinking capability lands.
        let provider = Arc::new(FakeProvider::new(1));
        let ai = AiService::new(provider.clone());
        let pool = unreachable_pool();

        let result = run_analysis(
            &ai,
            &pool,
            &UnusedEmbedder,
            "system",
            "user",
            AnalysisMode::Report,
        )
        .await
        .expect("run_analysis should succeed");

        assert_eq!(result, "final answer");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }
}
