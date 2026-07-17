//! Trigger/alerting rule model: `Rule`, `Operator`, `Severity`, `Problem`,
//! `ProblemStatus`.
//!
//! This module is the data model only — the evaluation loop
//! (`AlertingTask`) and the `rules`/`problems` migrations live elsewhere
//! (`crates/server/src/alerting.rs`, a separate migrations issue). Kept
//! here (not `crates/agent`) because both the server's alerting loop and
//! (eventually) API/UI layers need these types, matching every other
//! cross-cutting model in this crate (`transport::Topics`,
//! `storage::HostRow`).

use sqlx::FromRow;

/// A condition to evaluate against a host's latest value for one metric
/// key.
#[derive(Debug, Clone, FromRow)]
pub struct Rule {
    pub id: i64,
    /// `None` = applies to all hosts.
    pub host_id: Option<String>,
    pub item_key: String,
    pub operator: Operator,
    pub threshold: f64,
    pub severity: Severity,
    /// How long the condition must stay continuously true before a
    /// `Problem` is raised. `0` = trip immediately on the first true
    /// evaluation — matches Zabbix's trigger-duration concept.
    pub sustained_for_secs: i64,
    pub enabled: bool,
}

/// Comparison a [`Rule`] applies between a metric's latest value and its
/// `threshold`.
///
/// Stored as `TEXT` (not a native Postgres enum type), hence
/// `type_name = "text"` rather than naming a `CREATE TYPE`.
#[derive(Debug, Clone, Copy, PartialEq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Operator {
    GreaterThan,
    LessThan,
    Equal,
}

impl Operator {
    /// Evaluates `value` against `threshold` for this operator.
    ///
    /// `Equal` compares within `f64::EPSILON`, not `==` — metric values
    /// arrive as `f64` from arbitrary sources (collectors, manual API
    /// input), and exact bitwise equality on floats is the wrong tool for
    /// "is this value at the threshold" (e.g. `0.1 + 0.2 != 0.3` under
    /// `==`, despite being equal for any practical alerting purpose).
    pub fn evaluate(self, value: f64, threshold: f64) -> bool {
        match self {
            Operator::GreaterThan => value > threshold,
            Operator::LessThan => value < threshold,
            Operator::Equal => (value - threshold).abs() < f64::EPSILON,
        }
    }
}

/// How urgently a [`Problem`] should be treated.
#[derive(Debug, Clone, Copy, PartialEq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// Lifecycle state of a [`Problem`].
#[derive(Debug, Clone, Copy, PartialEq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ProblemStatus {
    Open,
    Resolved,
}

/// One instance of a [`Rule`]'s condition being true for a specific host —
/// opened when the condition first trips (after `sustained_for_secs`, if
/// any), resolved when the condition stops being true. At most one `Open`
/// problem exists per `(rule_id, host_id)` pair at a time (enforced by the
/// `rules`/`problems` migration's partial unique index, issue 8.2).
#[derive(Debug, Clone, FromRow)]
pub struct Problem {
    pub id: i64,
    pub rule_id: i64,
    pub host_id: String,
    pub status: ProblemStatus,
    /// Unix millis.
    pub opened_at: i64,
    pub resolved_at: Option<i64>,
    pub last_value: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greater_than() {
        assert!(Operator::GreaterThan.evaluate(91.0, 90.0));
        assert!(!Operator::GreaterThan.evaluate(90.0, 90.0));
        assert!(!Operator::GreaterThan.evaluate(89.0, 90.0));
    }

    #[test]
    fn less_than() {
        assert!(Operator::LessThan.evaluate(9.0, 10.0));
        assert!(!Operator::LessThan.evaluate(10.0, 10.0));
        assert!(!Operator::LessThan.evaluate(11.0, 10.0));
    }

    #[test]
    fn equal_exact_match() {
        assert!(Operator::Equal.evaluate(42.0, 42.0));
    }

    #[test]
    fn equal_is_false_outside_epsilon() {
        assert!(!Operator::Equal.evaluate(42.0001, 42.0));
        assert!(!Operator::Equal.evaluate(41.9, 42.0));
    }

    /// The exact reason `Equal` can't use `==`: this is a real, common
    /// floating-point representation artifact, not a contrived example —
    /// `0.1_f64 + 0.2_f64` is `0.30000000000000004`, not `0.3`.
    #[test]
    fn equal_handles_the_classic_point_one_plus_point_two_case() {
        let sum = 0.1_f64 + 0.2_f64;
        assert_ne!(sum, 0.3, "if this ever fails, f64 arithmetic changed");
        assert!(
            Operator::Equal.evaluate(sum, 0.3),
            "epsilon-based comparison should treat these as equal"
        );
    }
}
