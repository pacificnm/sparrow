import {
  Chip,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@nest/components";

import type { Host } from "../lib/api";

export type HostListProps = {
  hosts: Host[];
  selectedHostId?: string | null;
  onSelectHost: (hostId: string) => void;
};

/** Table of hosts with an online/offline badge; clicking a row selects it. */
export function HostList({ hosts, selectedHostId, onSelectHost }: HostListProps) {
  if (hosts.length === 0) {
    return (
      <Typography variant="body2" className="text-nest-muted">
        No hosts registered yet.
      </Typography>
    );
  }

  return (
    <Table>
      <TableHead>
        <TableRow>
          <TableCell component="th">Host</TableCell>
          <TableCell component="th">Status</TableCell>
          <TableCell component="th">Last seen</TableCell>
        </TableRow>
      </TableHead>
      <TableBody>
        {hosts.map((host) => (
          <TableRow
            key={host.host_id}
            hover
            onClick={() => onSelectHost(host.host_id)}
            className={
              selectedHostId === host.host_id
                ? "cursor-pointer bg-nest-surface"
                : "cursor-pointer"
            }
            data-testid={`host-row-${host.host_id}`}
          >
            <TableCell>
              <Typography variant="body2" className="font-medium">
                {host.hostname}
              </Typography>
              <Typography variant="caption" className="text-nest-muted">
                {host.host_id}
              </Typography>
            </TableCell>
            <TableCell>
              <Chip
                label={host.online ? "Online" : "Offline"}
                color={host.online ? "success" : "error"}
                size="small"
              />
            </TableCell>
            <TableCell>
              <Typography variant="body2">
                {new Date(host.last_seen_ms).toLocaleString()}
              </Typography>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
