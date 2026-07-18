import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { Problem } from "../lib/api";
import { AnalystPanel } from "./AnalystPanel";

function problem(overrides: Partial<Problem>): Problem {
  return {
    id: 1,
    rule_id: 1,
    host_id: "host-1",
    status: "open",
    opened_at: Date.now(),
    resolved_at: null,
    last_value: 95,
    severity: "warning",
    ...overrides,
  };
}

describe("AnalystPanel", () => {
  it("asks a free-form question and renders the response as Markdown", async () => {
    const onRunAnalysis = vi.fn().mockResolvedValue("**All clear.**");
    render(<AnalystPanel problems={[]} onRunAnalysis={onRunAnalysis} />);

    fireEvent.change(screen.getByLabelText("Ask a question"), {
      target: { value: "how is everything?" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));

    expect(onRunAnalysis).toHaveBeenCalledWith(null, "how is everything?", "quick");

    await waitFor(() => {
      expect(screen.getByTestId("analyst-response")).toBeInTheDocument();
    });
    // react-markdown renders **bold** as a real <strong>, not literal asterisks.
    expect(screen.getByText("All clear.").tagName).toBe("STRONG");
  });

  it("renders one explain action per open problem and runs it by host_id", async () => {
    const onRunAnalysis = vi.fn().mockResolvedValue("Disk usage is normal.");
    render(
      <AnalystPanel
        problems={[problem({ id: 1, host_id: "host-1" }), problem({ id: 2, host_id: "host-2" })]}
        onRunAnalysis={onRunAnalysis}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Explain problem on host-2" }));

    expect(onRunAnalysis).toHaveBeenCalledWith("host-2", null, "quick");
    await waitFor(() => {
      expect(screen.getByTestId("analyst-response")).toBeInTheDocument();
    });
  });

  it("shows an error message when onRunAnalysis rejects", async () => {
    const onRunAnalysis = vi.fn().mockRejectedValue(new Error("server unreachable"));
    render(<AnalystPanel problems={[]} onRunAnalysis={onRunAnalysis} />);

    fireEvent.change(screen.getByLabelText("Ask a question"), {
      target: { value: "hello" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));

    await waitFor(() => {
      expect(screen.getByText("server unreachable")).toBeInTheDocument();
    });
  });

  it("disables the Ask button while the question is empty", () => {
    render(<AnalystPanel problems={[]} onRunAnalysis={vi.fn()} />);

    expect(screen.getByRole("button", { name: "Ask" })).toBeDisabled();
  });
});
