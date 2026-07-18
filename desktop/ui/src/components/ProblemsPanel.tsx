import { Chip, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@nest/components";

import type { ChipColor } from "@nest/components";
import type { Problem, Severity } from "../lib/api";

export type ProblemsPanelProps = {
  problems: Problem[];
};

/** Chip color per Severity — no "critical" token exists in nest-react-theme's
 * palette (confirmed against core/crates/nest-react-theme/src/tailwind.rs:
 * background/foreground/primary/secondary/border/surface/accent/muted/
 * success/warning/error/info, nothing more granular), so Critical maps to
 * the most severe token available, "error". */
const SEVERITY_COLOR: Record<Severity, ChipColor> = {
  info: "info",
  warning: "warning",
  critical: "error",
};

/** Open Problems list, severity-colored. */
export function ProblemsPanel({ problems }: ProblemsPanelProps) {
  if (problems.length === 0) {
    return (
      <Typography variant="body2" className="text-nest-muted">
        No open problems.
      </Typography>
    );
  }

  return (
    <Table>
      <TableHead>
        <TableRow>
          <TableCell component="th">Severity</TableCell>
          <TableCell component="th">Host</TableCell>
          <TableCell component="th">Last value</TableCell>
          <TableCell component="th">Opened</TableCell>
        </TableRow>
      </TableHead>
      <TableBody>
        {problems.map((problem) => (
          <TableRow key={problem.id} data-testid={`problem-row-${problem.id}`}>
            <TableCell>
              <Chip
                label={problem.severity}
                color={SEVERITY_COLOR[problem.severity]}
                size="small"
                data-testid={`severity-chip-${problem.id}`}
              />
            </TableCell>
            <TableCell>{problem.host_id}</TableCell>
            <TableCell numeric>{problem.last_value}</TableCell>
            <TableCell>{new Date(problem.opened_at).toLocaleString()}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
