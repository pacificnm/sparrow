# Phase 10 Task Spec — AI Health Analyst

**Repo:** `pacificnm/sparrow`
**Crate:** `crates/core/src/analyst/` (tool implementations, agent loop) + `crates/server/src/api/analyst.rs` (API surface) + `desktop/` panel (Phase 11, not this phase)
**Prerequisite:** Phase 4 (storage), Phase 7 (server/API), Phase 8 (Problems), `nest-ai-claude` (Phase 3), `nest-ai-ollama` (already exists in the framework).

## Ground truth

- Neither `nest-ai` nor `nest-claude`/`nest-ai-claude` implement an agent
  loop — Sparrow builds it: send a `CompletionRequest` with `tools`, inspect
  `CompletionResponse.tool_calls`, execute them against Sparrow's own data,
  append results as `ChatMessage::tool_result(name, content)` turns, send
  again, repeat until `tool_calls` is empty.
- Provider is swappable at the config level: register either `OllamaModule`
  or `ClaudeAiModule` (Phase 3) — both register an `AiService` under the same
  service type, so `crates/server` code that calls `AiService` doesn't care
  which one is active. **Do not** write analyst code that imports
  `nest_ai_ollama` or `nest_ai_claude` directly — depend only on `nest_ai::AiService`/`AiProvider`, or the swap stops being free.
- `nest_data_postgres::VectorSearch::new(pool, table, id_col, embedding_col).with_project_scope(col)` and `.search_similar(&embedding, limit, scope) -> Vec<SimilarityHit>` — already built (Phase 1's ground truth), reusable as-is for incident similarity search.
- **Open research item, not resolved in this spec:** embedding generation.
  `nest-ai`'s `AiProvider` trait (confirmed in Phase 3's research) has no
  `embed()` method — only `complete`/`stream_complete`. Ollama's own HTTP API
  does expose `/api/embeddings` separately from `/api/chat`, but
  `nest-ai-ollama`'s current `OllamaProvider` (confirmed in Phase 3's
  research) does not wrap it. **Before starting this phase's embedding work,
  check whether `nest-ai-ollama` has since grown embedding support; if not,
  this phase needs to add a small `embed()` method to `OllamaProvider`
  directly (a new framework PR, same review bar as Phases 1–3) or call
  Ollama's `/api/embeddings` directly from Sparrow via a raw HTTP request as
  a stopgap.** Do not silently skip `search_similar_incidents` — it's the
  part of this feature that actually delivers on "help solve problems," not
  a nice-to-have.

---

## Design

### Tools (`crates/core/src/analyst/tools.rs`)

Four tools, defined once as `nest_ai::ToolDefinition`s and dispatched by name:

```rust
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

/// Executes one tool call against Sparrow's own data. Returns the result as
/// a JSON string — this becomes the content of the `ChatMessage::tool_result`
/// sent back to the model.
pub async fn execute_tool(
    call: &nest_ai::ToolCall,
    pool: &sqlx::PgPool,
    embedder: &dyn Embedder, // see the embedding-generation open item above
) -> String {
    let result = match call.name.as_str() {
        "get_host_status" => get_host_status(pool, &call.arguments).await,
        "get_metric_history" => get_metric_history(pool, &call.arguments).await,
        "get_active_problems" => get_active_problems(pool, &call.arguments).await,
        "search_similar_incidents" => search_similar_incidents(pool, embedder, &call.arguments).await,
        other => Err(format!("unknown tool: {other}")),
    };
    match result {
        Ok(value) => value.to_string(),
        Err(err) => serde_json::json!({ "error": err }).to_string(),
    }
}
```

Each `get_*`/`search_*` function: parse `call.arguments` (a `serde_json::Value`)
into a typed struct with `serde_json::from_value`, return a clear error
string (not a panic) on a malformed/missing argument — the model can and
will occasionally call a tool with a slightly wrong shape, especially on a
weaker local provider; the loop must survive that, not crash.

**On failure never returning `Err` up through `execute_tool` itself:** the
function signature above returns `String` unconditionally, folding tool
execution errors into the JSON payload sent back to the model (`{"error":
"..."}"`) rather than propagating a Rust `Result` out of the agent loop. This
is deliberate — a failed tool call is normal conversational flow (the model
should see the error and can retry or explain it to the user), not an
application-level failure.

### Agent loop (`crates/core/src/analyst/loop.rs`)

```rust
const MAX_TOOL_ROUNDS: usize = 6; // hard cap — never let a bad prompt spin forever

pub async fn run_analysis(
    ai: &nest_ai::AiService,
    pool: &sqlx::PgPool,
    embedder: &dyn Embedder,
    system_prompt: &str,
    user_prompt: &str,
    thinking_effort: AnalysisMode,
) -> NestResult<String> {
    let mut messages = vec![
        nest_ai::ChatMessage::system(system_prompt),
        nest_ai::ChatMessage::user(user_prompt),
    ];

    for _round in 0..MAX_TOOL_ROUNDS {
        let request = nest_ai::CompletionRequest {
            model: None, // provider default
            messages: messages.clone(),
            format: None,
            tools: crate::analyst::tools::tool_definitions(),
        };
        // CHECK: does AiService expose the AiProvider it wraps directly (e.g.
        // `ai.provider().complete(request)`), or does AiService itself implement
        // `complete`? Confirm the exact accessor against AiService's real definition
        // (core/crates/nest-ai/src/service.rs — not pulled in this research pass)
        // before writing this call.
        let response = ai.complete(request).await.map_err(/* AiError -> NestError, check the From impl exists or write one */)?;

        if response.tool_calls.is_empty() {
            return Ok(response.content);
        }

        messages.push(nest_ai::ChatMessage::assistant_tool_calls(response.tool_calls.clone()));
        for call in &response.tool_calls {
            let result = crate::analyst::tools::execute_tool(call, pool, embedder).await;
            messages.push(nest_ai::ChatMessage::tool_result(&call.name, result));
        }
    }

    Err(/* NestError: "analysis exceeded max tool-call rounds" */)
}
```

**`AnalysisMode` — the two modes from the plan:**

```rust
pub enum AnalysisMode {
    /// Fast per-Problem explanation — no extended thinking, whichever
    /// provider is configured runs its normal (cheap/fast) path.
    Quick,
    /// Slower periodic health-trend report. Only meaningfully different when
    /// the active provider is `nest-ai-claude` (extended thinking/`Effort::High`
    /// per Phase 3's design table) — `nest-ai-ollama` has no equivalent concept,
    /// so this mode should degrade gracefully to "same as Quick" rather than
    /// erroring when Ollama is the active provider. Implement that degradation
    /// explicitly (a match on provider_id() or a capability flag), don't let it
    /// silently do nothing or silently error.
}
```

`nest_ai::CompletionRequest` (confirmed shape from Phase 3's research) has no
generic "effort"/"thinking" field — this was flagged as an open item back in
the original plan (§2's `nest-ai-claude` design table: *"doesn't have an
obvious nest-ai-generic equivalent yet"*). For this phase, the pragmatic
answer: if a generic field hasn't been added to `nest_ai::CompletionRequest`
by the time this phase starts, implement `AnalysisMode::Report`'s
extended-thinking behavior as a `ClaudeAiProvider`-specific escape hatch
(check whether Phase 3 ended up adding one) rather than blocking this whole
phase on a `nest-ai` core-crate change. Note whichever approach was actually
taken in this crate's doc comments — a future reader needs to know which of
the two options got picked.

### `search_similar_incidents` implementation

```rust
async fn search_similar_incidents(pool: &PgPool, embedder: &dyn Embedder, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let description = args.get("description").and_then(|v| v.as_str())
        .ok_or("missing `description` argument")?;
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(5) as usize;

    let embedding = embedder.embed(description).await.map_err(|e| e.to_string())?;
    let search = nest_data_postgres::VectorSearch::new(pool.clone(), "resolved_incidents", "id", "embedding");
    let hits = search.search_similar(&embedding, limit, None).await.map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(hits).map_err(|e| e.to_string())?)
}
```

New migration (add to Phase 4's list): `resolved_incidents` table storing
`{id, host_id, problem_description, resolution_notes, embedding vector(N)}`,
`N` matching whichever embedding model gets chosen (check the actual
dimension from the embedding provider before hardcoding `N` — Phase 1's
research noted `nest-data-postgres`'s own default is 1536 for OpenAI's
`text-embedding-3-small`, which is almost certainly the wrong dimension if
the embedding step ends up going through Ollama instead; **do not copy that
default blindly**).

Populate `resolved_incidents` when a Problem transitions to `Resolved`
(Phase 8) — hook this into `resolve_problem`, generating an embedding of the
Problem's description at resolution time (not at query time, to keep query
latency low), plus whatever resolution notes exist (Sparrow doesn't yet have
a UI for entering resolution notes — for v1, this can just be the rule's
`item_key`/threshold/duration as a synthesized description; a real
free-text resolution-notes field is a reasonable follow-up, not required for
this phase to be useful).

### API (`crates/server/src/api/analyst.rs`)

```rust
pub fn routes() -> nest_http_serve::RouteGroup {
    nest_http_serve::RouteGroup::new("/api")
        .post("/analyst/run", run_analysis_handler)
}

#[derive(serde::Deserialize)]
struct RunAnalysisRequest {
    host_id: Option<String>,
    /// Free-form question. If absent and `host_id` is present, this is the
    /// "explain this Problem" quick action — synthesize a default prompt
    /// from that host's currently open Problem(s) rather than requiring the
    /// caller to phrase a question.
    question: Option<String>,
    #[serde(default)]
    mode: AnalysisModeWire, // "quick" | "report" -> crate::analyst::loop::AnalysisMode
}

async fn run_analysis_handler(ctx: nest_http_serve::RequestContext) -> nest_http_serve::HttpResult {
    // Parse the request body into RunAnalysisRequest, build the system/user
    // prompts (see the two cases in the struct's doc comment above), call
    // this phase's crate::analyst::loop::run_analysis(...) with the
    // configured AiService, the server's pool, and the resolved Embedder
    // (Issue 10.1), return the response text as JSON.
    // Verify RequestContext's body-parsing/typed-JSON-response methods
    // against context.rs first — same "verify, don't guess" instruction as
    // every other api/ handler in this plan (Phase 7's api/hosts.rs, Phase
    // 9's api/agent_config.rs).
    todo!()
}
```

This is the endpoint Phase 11's `AnalystPanel.tsx` calls (`run_analysis`
IPC command). Implementation and its test are tracked under Milestone 11
(Issue 11.2) since that's when the desktop dashboard first needs it, but
the contract lives here — alongside the agent loop it wraps — so it isn't
an undocumented Phase-11-only assumption.

---

## Tests

- `tools.rs`: each `get_*` function against seeded `testcontainers` Postgres data — correct results, and a malformed-argument case returning an error string, not a panic.
- `loop.rs`: use a **fake `AiProvider`** test double (scripted to return a tool call on round 1, a final answer on round 2) rather than a real Ollama/Claude call — proves the loop's control flow (tool execution → message history growth → termination) without needing a live model. Also test the `MAX_TOOL_ROUNDS` cap with a fake provider that always requests a tool call, confirm the loop terminates with an error rather than spinning forever.
- `search_similar_incidents`: seed `resolved_incidents` with a couple of fake embeddings (don't need a real embedding model for this test — synthetic vectors of the right dimension are enough to prove the pgvector query shape works), assert the closest one comes back first.

**Acceptance:** `cargo test -p sparrow-core analyst::` passes with Docker running. Manual/integration acceptance per the plan: the same analysis request run against both `nest-ai-ollama` and `nest-ai-claude` (config swap only) produces a valid response from each.

## Explicit "do not" list

- Do not import `nest_ai_ollama`/`nest_ai_claude` directly in analyst code — only `nest_ai`.
- Do not let a tool-execution error propagate as a Rust `Err` out of the agent loop — fold it into the tool-result JSON per the design above.
- Do not hardcode the pgvector embedding dimension to `nest-data-postgres`'s 1536 default without confirming it matches the actual embedding provider in use.
- Do not skip resolving the embedding-generation gap silently — it's flagged as an explicit research item at the top of this spec for a reason.
