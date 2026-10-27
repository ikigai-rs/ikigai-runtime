//! Budgets: fuel, an epoch deadline and a memory ceiling, and the **meet** that combines them
//! (R-D10).
//!
//! The effective budget for a run is the minimum of the grant, the bundle, the host and the
//! caller. A `None` field is "no opinion" — the meet of `Some(a)` and `None` is `Some(a)`, and
//! the meet of two bounds is the smaller. There is **no silent unlimited default**: a host that
//! sets no ceiling at all refuses to start the runtime (J2's rule); that refusal lives in the
//! engine, not in this type, so that [`Limits::unbounded`] can be named explicitly where an
//! outer bound is already known to exist.

use core::time::Duration;

/// Where a bound came from, which decides how its exhaustion is reported (R-D10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// A caller/grant/bundle budget. Exhaustion is `Error::OverBudget` — never retried; only a
    /// holder of a larger bound can clear it.
    Budget,
    /// A deadline the host imposes for its own reasons. Exhaustion is `Error::Timeout`.
    Host,
}

/// An epoch deadline with its provenance, so the engine maps its exhaustion to the right error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deadline {
    /// Wall-clock duration from the start of the call.
    pub after: Duration,
    /// Whether it is a budget (→ `OverBudget`) or a host deadline (→ `Timeout`).
    pub provenance: Provenance,
}

impl Deadline {
    /// A budget deadline — its exhaustion is `OverBudget`.
    pub fn budget(after: Duration) -> Self {
        Deadline {
            after,
            provenance: Provenance::Budget,
        }
    }

    /// A host deadline — its exhaustion is `Timeout`.
    pub fn host(after: Duration) -> Self {
        Deadline {
            after,
            provenance: Provenance::Host,
        }
    }

    /// The tighter of two deadlines. A budget deadline wins ties, since an `OverBudget` refusal
    /// is the one a caller can act on by asking for a larger bound.
    fn meet(self, other: Deadline) -> Deadline {
        let other_wins = other.after < self.after
            || (other.after == self.after && other.provenance == Provenance::Budget);
        if other_wins {
            other
        } else {
            self
        }
    }
}

/// The compute budget for one run: fuel (a deterministic cost), an epoch deadline (wall-clock
/// preemption) and a memory ceiling. Every field is optional so the [`meet`](Limits::meet) can
/// combine opinions, but the engine refuses a run with no bound of any kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// `urn:cap:compute:fuel:<n>` — units of deterministic compute. Exhaustion is `OverBudget`.
    pub fuel: Option<u64>,
    /// The epoch deadline (`urn:cap:compute:time:<ms>` or a host ceiling), with its provenance.
    pub deadline: Option<Deadline>,
    /// `urn:cap:compute:memory:<bytes>` — the linear-memory ceiling (`StoreLimits`).
    pub memory_bytes: Option<usize>,
    /// The guest stack ceiling (`max_wasm_stack`), so an overflow is a trap, never a process
    /// abort. A host setting; carried here so the meet keeps the tightest.
    pub max_wasm_stack: Option<usize>,
}

impl Limits {
    /// No bound at all. **Named** so nobody reaches for it by accident — the engine refuses to
    /// run under it (there is no silent unlimited default). Use it only where an OUTER bound is
    /// known to exist (for example a native build under the capability clamp, R-D9).
    pub fn unbounded() -> Self {
        Limits::default()
    }

    /// A fuel budget alone.
    pub fn fuel(fuel: u64) -> Self {
        Limits {
            fuel: Some(fuel),
            ..Limits::default()
        }
    }

    /// A deadline alone.
    pub fn deadline(deadline: Deadline) -> Self {
        Limits {
            deadline: Some(deadline),
            ..Limits::default()
        }
    }

    /// The budget a caller's grants state (R-D10): `urn:cap:compute:fuel:<n>`,
    /// `urn:cap:compute:memory:<bytes>` and `urn:cap:compute:time:<ms>` (a BUDGET deadline, so its
    /// exhaustion is `OverBudget`). Holding several grants of one family means the caller may use
    /// the largest, so that one is its opinion; a family it holds none of is no opinion, as is
    /// root (the host ceiling still bounds the run). A malformed number grants nothing.
    ///
    /// ```
    /// use ikigai_runtime::{Grants, Limits};
    ///
    /// let caller = Grants::from_scopes([
    ///     "urn:cap:compute:fuel:1000",
    ///     "urn:cap:compute:fuel:5000",
    ///     "urn:cap:compute:time:250",
    /// ]);
    /// let limits = Limits::from_grants(&caller);
    /// assert_eq!(limits.fuel, Some(5000));
    /// assert_eq!(limits.deadline.unwrap().after.as_millis(), 250);
    /// assert_eq!(limits.memory_bytes, None);
    /// assert_eq!(Limits::from_grants(&Grants::Root), Limits::unbounded());
    /// ```
    pub fn from_grants(grants: &crate::Grants) -> Limits {
        let scopes = match grants {
            crate::Grants::Root => return Limits::unbounded(),
            crate::Grants::Scoped(scopes) => scopes,
        };
        let largest = |family: &str| -> Option<u64> {
            scopes
                .iter()
                .filter_map(|scope| scope.strip_prefix(family))
                .filter_map(|n| n.parse::<u64>().ok())
                .max()
        };
        Limits {
            fuel: largest("urn:cap:compute:fuel:"),
            deadline: largest("urn:cap:compute:time:")
                .map(|ms| Deadline::budget(Duration::from_millis(ms))),
            memory_bytes: largest("urn:cap:compute:memory:")
                .map(|b| usize::try_from(b).unwrap_or(usize::MAX)),
            max_wasm_stack: None,
        }
    }

    /// Whether any bound is set. The engine refuses a run where this is `false`.
    pub fn is_bounded(&self) -> bool {
        self.fuel.is_some() || self.deadline.is_some() || self.memory_bytes.is_some()
    }

    /// Builder: set the memory ceiling.
    pub fn with_memory(mut self, bytes: usize) -> Self {
        self.memory_bytes = Some(bytes);
        self
    }

    /// Builder: set the stack ceiling.
    pub fn with_stack(mut self, bytes: usize) -> Self {
        self.max_wasm_stack = Some(bytes);
        self
    }

    /// Builder: set the deadline.
    pub fn with_deadline(mut self, deadline: Deadline) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// The **meet** of two budgets: field by field, the tighter opinion wins, and `None` yields
    /// to `Some`. This is how a run's effective budget is formed from the grant, the bundle, the
    /// host and the caller (R-D10): `grant.meet(&bundle).meet(&host).meet(&caller)`.
    pub fn meet(&self, other: &Limits) -> Limits {
        Limits {
            fuel: meet_min(self.fuel, other.fuel),
            deadline: match (self.deadline, other.deadline) {
                (Some(a), Some(b)) => Some(a.meet(b)),
                (a, b) => a.or(b),
            },
            memory_bytes: meet_min(self.memory_bytes, other.memory_bytes),
            max_wasm_stack: meet_min(self.max_wasm_stack, other.max_wasm_stack),
        }
    }
}

/// The meet of two optional bounds: `None` is "no opinion", so it yields to `Some`; two bounds
/// meet at the smaller.
fn meet_min<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meet_takes_the_tighter_fuel_and_none_yields_to_some() {
        let grant = Limits::fuel(1_000);
        let caller = Limits::fuel(400);
        assert_eq!(grant.meet(&caller).fuel, Some(400));
        // None yields to Some.
        assert_eq!(Limits::unbounded().meet(&grant).fuel, Some(1_000));
        assert_eq!(grant.meet(&Limits::unbounded()).fuel, Some(1_000));
    }

    #[test]
    fn meet_combines_every_field_independently() {
        let a = Limits::fuel(100)
            .with_memory(4096)
            .with_deadline(Deadline::host(Duration::from_millis(50)));
        let b = Limits::fuel(200)
            .with_memory(2048)
            .with_deadline(Deadline::budget(Duration::from_millis(80)));
        let m = a.meet(&b);
        assert_eq!(m.fuel, Some(100));
        assert_eq!(m.memory_bytes, Some(2048));
        // 50ms host deadline is tighter than the 80ms budget deadline.
        assert_eq!(m.deadline, Some(Deadline::host(Duration::from_millis(50))));
    }

    #[test]
    fn a_budget_deadline_wins_a_tie_so_the_refusal_is_actionable() {
        let host = Deadline::host(Duration::from_millis(100));
        let budget = Deadline::budget(Duration::from_millis(100));
        assert_eq!(host.meet(budget).provenance, Provenance::Budget);
        assert_eq!(budget.meet(host).provenance, Provenance::Budget);
    }

    #[test]
    fn unbounded_is_not_bounded_and_a_single_bound_is() {
        assert!(!Limits::unbounded().is_bounded());
        assert!(Limits::fuel(1).is_bounded());
        assert!(Limits::deadline(Deadline::host(Duration::from_millis(1))).is_bounded());
        assert!(Limits::unbounded().with_memory(1).is_bounded());
    }
}
