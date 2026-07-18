import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { Host } from "../lib/api";
import { HostList } from "./HostList";

const hosts: Host[] = [
  {
    host_id: "host-1",
    hostname: "web-01",
    online: true,
    last_seen_ms: Date.now(),
  },
  {
    host_id: "host-2",
    hostname: "web-02",
    online: false,
    last_seen_ms: Date.now(),
  },
];

describe("HostList", () => {
  it("renders a row per host with its online/offline badge", () => {
    render(<HostList hosts={hosts} onSelectHost={() => {}} />);

    expect(screen.getByText("web-01")).toBeInTheDocument();
    expect(screen.getByText("web-02")).toBeInTheDocument();
    expect(screen.getByText("Online")).toBeInTheDocument();
    expect(screen.getByText("Offline")).toBeInTheDocument();
  });

  it("calls onSelectHost with the clicked host's id", () => {
    const onSelectHost = vi.fn();
    render(<HostList hosts={hosts} onSelectHost={onSelectHost} />);

    fireEvent.click(screen.getByTestId("host-row-host-2"));

    expect(onSelectHost).toHaveBeenCalledWith("host-2");
  });

  it("shows a friendly message when there are no hosts", () => {
    render(<HostList hosts={[]} onSelectHost={() => {}} />);

    expect(screen.getByText(/no hosts registered yet/i)).toBeInTheDocument();
  });
});
