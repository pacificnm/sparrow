//! AI Health Analyst (Phase 10): tool implementations and the agent loop.

pub mod embedder;
// `loop` is a reserved keyword, so the module is declared via its raw
// identifier (`r#loop`) — the file itself is still plainly `loop.rs`,
// matching the phase-10 spec's path exactly.
pub mod r#loop;
pub mod tools;
