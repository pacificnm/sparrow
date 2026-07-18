/**
 * Wrappers around Sparrow's own Tauri IPC commands.
 *
 * Registered as a Tauri **plugin** (`desktop/src-tauri/src/commands/mod.rs`,
 * Issue 11.0/11.1's finding), so these are invoked as `plugin:sparrow|name`,
 * not a bare command name.
 *
 * Command **arguments**: Tauri's `#[tauri::command]` macro converts
 * snake_case Rust parameter names to camelCase by default (verified against
 * `tauri-macros`' own source — no `rename_all = "snake_case"` override
 * exists on any of these commands), so `host_id` becomes `hostId` here.
 * Response **bodies** are unaffected by that — they're plain `serde_json`
 * output from Rust structs with no `rename_all` of their own, so they stay
 * snake_case (`host_id`, `last_seen_ms`, etc.), matching the field names
 * below exactly.
 */
import { invoke } from "@tauri-apps/api/core";

export type Host = {
  host_id: string;
  hostname: string;
  online: boolean;
  last_seen_ms: number;
};

export type MetricItem = {
  collector: string;
  key: string;
  value: string;
  value_type: "float" | "integer" | "text";
  tags: Record<string, string>;
  ts: number;
};

export type Severity = "info" | "warning" | "critical";
export type ProblemStatus = "open" | "resolved";

export type Problem = {
  id: number;
  rule_id: number;
  host_id: string;
  status: ProblemStatus;
  opened_at: number;
  resolved_at: number | null;
  last_value: number;
  severity: Severity;
};

export type AnalysisMode = "quick" | "report";

export async function listHosts(): Promise<Host[]> {
  return invoke<Host[]>("plugin:sparrow|list_hosts");
}

export async function getHostItems(hostId: string): Promise<MetricItem[]> {
  return invoke<MetricItem[]>("plugin:sparrow|get_host_items", { hostId });
}

export async function getActiveProblems(hostId?: string): Promise<Problem[]> {
  return invoke<Problem[]>("plugin:sparrow|get_active_problems", {
    hostId: hostId ?? null,
  });
}

export async function runAnalysis(
  hostId: string | null,
  question: string | null,
  mode: AnalysisMode,
): Promise<string> {
  return invoke<string>("plugin:sparrow|run_analysis", {
    hostId,
    question,
    mode,
  });
}
