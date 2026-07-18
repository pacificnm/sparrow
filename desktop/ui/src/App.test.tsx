import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { App } from "./App";

const invokeMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

const HOSTS = [
  { host_id: "host-1", hostname: "web-01", online: true, last_seen_ms: Date.now() },
  { host_id: "host-2", hostname: "web-02", online: false, last_seen_ms: Date.now() },
];

const PROBLEMS = [
  {
    id: 1,
    rule_id: 1,
    host_id: "host-1",
    status: "open",
    opened_at: Date.now(),
    resolved_at: null,
    last_value: 95,
    severity: "critical",
  },
];

const ITEMS = [
  {
    collector: "cpu",
    key: "cpu.usage_percent",
    value: "42.5",
    value_type: "float",
    tags: {},
    ts: Date.now(),
  },
];

/** Routes the mocked `invoke` by command name, same as the real Tauri IPC
 * boundary would — App.tsx doesn't know or care that these are canned
 * responses rather than a real desktop host. */
function mockInvokeImplementation() {
  invokeMock.mockImplementation(async (command: string) => {
    switch (command) {
      case "nest_theme_css":
        return { id: "cbre-light", mode: "light", variables: {}, root_block: "" };
      case "plugin:sparrow|list_hosts":
        return HOSTS;
      case "plugin:sparrow|get_active_problems":
        return PROBLEMS;
      case "plugin:sparrow|get_host_items":
        return ITEMS;
      case "plugin:sparrow|run_analysis":
        return "all clear";
      default:
        throw new Error(`unexpected invoke command: ${command}`);
    }
  });
}

describe("App", () => {
  it("loads hosts and problems on mount and renders them", async () => {
    mockInvokeImplementation();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText("web-01")).toBeInTheDocument();
    });
    expect(screen.getByText("web-02")).toBeInTheDocument();
    expect(screen.getByTestId("severity-chip-1")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("plugin:sparrow|list_hosts");
    expect(invokeMock).toHaveBeenCalledWith("plugin:sparrow|get_active_problems", {
      hostId: null,
    });
  });

  it("fetches and shows a host's items when a host row is selected", async () => {
    mockInvokeImplementation();
    render(<App />);

    await waitFor(() => expect(screen.getByText("web-01")).toBeInTheDocument());
    fireEvent.click(screen.getByTestId("host-row-host-1"));

    await waitFor(() => {
      expect(screen.getByText("cpu.usage_percent")).toBeInTheDocument();
    });
    expect(invokeMock).toHaveBeenCalledWith("plugin:sparrow|get_host_items", {
      hostId: "host-1",
    });
  });

  it("shows a load error when the initial host fetch fails", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "plugin:sparrow|list_hosts") {
        throw new Error("server unreachable");
      }
      if (command === "nest_theme_css") {
        return { id: "cbre-light", mode: "light", variables: {}, root_block: "" };
      }
      return [];
    });
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText("server unreachable")).toBeInTheDocument();
    });
  });

  it("runs an analysis via the AI Health Analyst panel using the live Problems list", async () => {
    mockInvokeImplementation();
    render(<App />);

    await waitFor(() => expect(screen.getByText("web-01")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Explain problem on host-1" }));

    await waitFor(() => {
      expect(screen.getByTestId("analyst-response")).toBeInTheDocument();
    });
    expect(invokeMock).toHaveBeenCalledWith("plugin:sparrow|run_analysis", {
      hostId: "host-1",
      question: null,
      mode: "quick",
    });
  });
});
