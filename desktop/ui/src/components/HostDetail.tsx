import { Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@nest/components";

import type { MetricItem } from "../lib/api";

export type HostDetailProps = {
  hostId: string;
  items: MetricItem[];
};

/** Latest values for one host, grouped by collector. */
export function HostDetail({ hostId, items }: HostDetailProps) {
  const groups = groupByCollector(items);
  const collectors = Object.keys(groups).sort();

  return (
    <div className="flex flex-col gap-6">
      <Typography variant="h6">{hostId}</Typography>

      {collectors.length === 0 && (
        <Typography variant="body2" className="text-nest-muted">
          No metric data for this host yet.
        </Typography>
      )}

      {collectors.map((collector) => (
        <div key={collector} data-testid={`collector-group-${collector}`}>
          <Typography variant="subtitle2" className="mb-2 capitalize">
            {collector}
          </Typography>
          <Table>
            <TableHead>
              <TableRow>
                <TableCell component="th">Key</TableCell>
                <TableCell component="th">Value</TableCell>
                <TableCell component="th">Updated</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {groups[collector].map((item) => (
                <TableRow key={item.key}>
                  <TableCell>{item.key}</TableCell>
                  <TableCell numeric={item.value_type !== "text"}>{item.value}</TableCell>
                  <TableCell>{new Date(item.ts).toLocaleTimeString()}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      ))}
    </div>
  );
}

function groupByCollector(items: MetricItem[]): Record<string, MetricItem[]> {
  const groups: Record<string, MetricItem[]> = {};
  for (const item of items) {
    (groups[item.collector] ??= []).push(item);
  }
  return groups;
}
