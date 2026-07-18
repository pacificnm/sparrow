import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { Problem } from "../lib/api";
import { ProblemsPanel } from "./ProblemsPanel";

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

describe("ProblemsPanel", () => {
  it("renders a row per problem with its severity chip", () => {
    render(
      <ProblemsPanel
        problems={[
          problem({ id: 1, severity: "critical" }),
          problem({ id: 2, severity: "info" }),
        ]}
      />,
    );

    expect(screen.getByTestId("problem-row-1")).toBeInTheDocument();
    expect(screen.getByTestId("problem-row-2")).toBeInTheDocument();
    expect(screen.getByText("critical")).toBeInTheDocument();
    expect(screen.getByText("info")).toBeInTheDocument();
  });

  it("maps critical severity to the error chip color", () => {
    render(<ProblemsPanel problems={[problem({ id: 1, severity: "critical" })]} />);

    expect(screen.getByTestId("severity-chip-1")).toHaveClass("bg-nest-error");
  });

  it("shows a friendly message when there are no open problems", () => {
    render(<ProblemsPanel problems={[]} />);

    expect(screen.getByText(/no open problems/i)).toBeInTheDocument();
  });
});
