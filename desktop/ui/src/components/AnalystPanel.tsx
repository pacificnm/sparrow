import { useState } from "react";

import { Alert, Button, CircularProgress, TextField, Typography } from "@nest/components";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type { AnalysisMode, Problem } from "../lib/api";

export type AnalystPanelProps = {
  /** Open problems, one "Explain" quick action rendered per row. */
  problems: Problem[];
  /**
   * Injected rather than importing `runAnalysis` from `lib/api.ts` directly
   * — keeps this component testable without mocking the Tauri IPC bridge,
   * same reasoning as `desktop/src-tauri`'s own `_via` function split
   * (Issue 11.1).
   */
  onRunAnalysis: (
    hostId: string | null,
    question: string | null,
    mode: AnalysisMode,
  ) => Promise<string>;
};

/**
 * Free-form question box plus a per-Problem "explain this Problem" quick
 * action, rendering `run_analysis`'s response as Markdown.
 *
 * Rendered as Markdown: `nest_ai::CompletionResponse.content` has no
 * documented plain-text-only convention anywhere in `nest_ai`'s own source
 * (grepped for "markdown": zero hits there), but the one place this
 * framework *does* render LLM-style output today (`ui/src/components/
 * MarkdownViewer.tsx`, the pacificnm/nest monorepo's own dev-tool UI) uses
 * `react-markdown` + `remark-gfm` — reused here rather than inventing a
 * second convention, and Markdown degrades gracefully to readable plain
 * text if a provider's response happens to contain none of it.
 */
export function AnalystPanel({ problems, onRunAnalysis }: AnalystPanelProps) {
  const [question, setQuestion] = useState("");
  const [mode] = useState<AnalysisMode>("quick");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [response, setResponse] = useState<string | null>(null);

  async function run(hostId: string | null, freeformQuestion: string | null) {
    setLoading(true);
    setError(null);
    setResponse(null);
    try {
      const result = await onRunAnalysis(hostId, freeformQuestion, mode);
      setResponse(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-2">
        <TextField
          label="Ask a question"
          multiline
          rows={3}
          value={question}
          onChange={(event) => setQuestion(event.target.value)}
          placeholder="e.g. why is web-01's CPU usage high?"
        />
        <div>
          <Button
            variant="contained"
            disabled={loading || question.trim() === ""}
            onClick={() => run(null, question)}
          >
            Ask
          </Button>
        </div>
      </div>

      {problems.length > 0 && (
        <div>
          <Typography variant="subtitle2" className="mb-2">
            Explain a problem
          </Typography>
          <div className="flex flex-col items-start gap-1">
            {problems.map((problem) => (
              <Button
                key={problem.id}
                variant="outlined"
                size="small"
                disabled={loading}
                onClick={() => run(problem.host_id, null)}
              >
                Explain problem on {problem.host_id}
              </Button>
            ))}
          </div>
        </div>
      )}

      {loading && (
        <div data-testid="analyst-loading">
          <CircularProgress size="small" />
        </div>
      )}
      {error && <Alert severity="error">{error}</Alert>}
      {response && (
        <article
          className="nest-rich-text rounded-nest-md border border-nest-border bg-nest-surface p-4"
          data-testid="analyst-response"
        >
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{response}</ReactMarkdown>
        </article>
      )}
    </div>
  );
}
