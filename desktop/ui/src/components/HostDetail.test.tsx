import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { MetricItem } from "../lib/api";
import { HostDetail } from "./HostDetail";

const items: MetricItem[] = [
  {
    collector: "cpu",
    key: "cpu.usage_percent",
    value: "42.5",
    value_type: "float",
    tags: {},
    ts: Date.now(),
  },
  {
    collector: "memory",
    key: "memory.used_bytes",
    value: "123456",
    value_type: "integer",
    tags: {},
    ts: Date.now(),
  },
  {
    collector: "cpu",
    key: "cpu.governor",
    value: "performance",
    value_type: "text",
    tags: {},
    ts: Date.now(),
  },
];

describe("HostDetail", () => {
  it("groups items by collector", () => {
    render(<HostDetail hostId="host-1" items={items} />);

    expect(screen.getByTestId("collector-group-cpu")).toBeInTheDocument();
    expect(screen.getByTestId("collector-group-memory")).toBeInTheDocument();
  });

  it("renders each item's key and value", () => {
    render(<HostDetail hostId="host-1" items={items} />);

    expect(screen.getByText("cpu.usage_percent")).toBeInTheDocument();
    expect(screen.getByText("42.5")).toBeInTheDocument();
    expect(screen.getByText("memory.used_bytes")).toBeInTheDocument();
    expect(screen.getByText("123456")).toBeInTheDocument();
  });

  it("shows a friendly message when there is no data", () => {
    render(<HostDetail hostId="host-1" items={[]} />);

    expect(screen.getByText(/no metric data for this host yet/i)).toBeInTheDocument();
  });
});
