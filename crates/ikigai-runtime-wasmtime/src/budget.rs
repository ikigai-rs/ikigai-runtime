//! What both tiers share: the engine configuration, the epoch ticker, the memory limiter, the
//! budget a store starts with, and the R-D10 error mapping.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use ikigai_core::Error;
use ikigai_runtime::{Consumed, EngineError, Limits, Provenance};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap};

/// The epoch cadence: the engine's epoch is incremented every millisecond, so an epoch deadline
/// of `n` ticks is `n` milliseconds (R1 measured 10 ms ticks; 1 ms is used here for test
/// responsiveness).
pub(crate) const TICK: Duration = Duration::from_millis(1);

/// How an engine meters a run (R-D10). Fuel is a deterministic cost and the default; R1 and the
/// Config docs put it at up to 2-3× the run time of an epoch-only engine, so a host whose runs
/// need only a wall-clock bound can build the cheaper engine. A completed run's answer does not
/// depend on how it was metered, so both are equally cacheable (R-D10's correction of R1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Metering {
    /// Fuel and epochs: the deterministic cost plus the wall-clock deadline.
    #[default]
    FuelAndEpoch,
    /// Epochs only. A fuel budget in the limits is then refused at the start of the run, never
    /// silently ignored.
    EpochOnly,
}

/// Build a wasmtime engine for `ceiling` and `metering`. **There is no silent unlimited
/// default**: an unbounded ceiling is refused here, so the runtime "refuses to start" (R-D10,
/// J2's rule); an epoch-only engine also needs a ceiling bound it can enforce (a deadline or a
/// memory ceiling), since it cannot enforce fuel.
pub(crate) fn engine_for(ceiling: &Limits, metering: Metering) -> Result<Engine, EngineError> {
    if !ceiling.is_bounded() {
        return Err(EngineError::Unbounded);
    }
    if metering == Metering::EpochOnly
        && ceiling.deadline.is_none()
        && ceiling.memory_bytes.is_none()
    {
        return Err(EngineError::Unbounded);
    }
    let mut config = Config::new();
    config.consume_fuel(metering == Metering::FuelAndEpoch);
    config.epoch_interruption(true);
    // Relaxed SIMD only in its deterministic mode, so a result never depends on the host and
    // caching stays sound (R-D12).
    config.relaxed_simd_deterministic(true);
    if let Some(stack) = ceiling.max_wasm_stack {
        config.max_wasm_stack(stack);
    }
    Engine::new(&config).map_err(|e| EngineError::Compile(format!("{e:#}")))
}

/// A memory limiter that denies growth past a ceiling and records that it did, so a trap after a
/// denied growth is reported as `OverBudget` rather than a generic endpoint failure.
pub(crate) struct MemLimiter {
    max_bytes: Option<usize>,
    pub(crate) denied: bool,
}

impl MemLimiter {
    pub(crate) fn new(max_bytes: Option<usize>) -> Self {
        MemLimiter {
            max_bytes,
            denied: false,
        }
    }
}

impl ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        if let Some(max) = self.max_bytes {
            if desired > max {
                self.denied = true;
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        Ok(true)
    }
}

/// A background thread that increments the engine's epoch on a fixed cadence, so epoch deadlines
/// fire. It stops and is joined when the engine drops.
pub(crate) struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    pub(crate) fn start(engine: &Engine) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let engine = engine.clone();
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                engine.increment_epoch();
            }
        });
        EpochTicker {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Refuse a run whose limits ask for something the engine cannot enforce: a fuel budget on an
/// epoch-only engine. Refused, never silently dropped (a budget nobody enforces makes the grant
/// lie).
pub(crate) fn check_metering(limits: &Limits, metering: Metering) -> Result<(), Error> {
    if metering == Metering::EpochOnly && limits.fuel.is_some() {
        return Err(Error::InvalidArgument {
            name: "fuel".into(),
            detail: "this engine meters by epoch only and cannot enforce a fuel budget".into(),
        });
    }
    Ok(())
}

/// Set the store's fuel and epoch deadline from the effective limits. With fuel metering on, a
/// run with no fuel bound is given `u64::MAX` and governed by its deadline instead (the engine
/// ceiling guarantees at least one real bound).
pub(crate) fn apply_budget<T>(store: &mut Store<T>, limits: &Limits, metering: Metering) {
    if metering == Metering::FuelAndEpoch {
        let _ = store.set_fuel(limits.fuel.unwrap_or(u64::MAX));
    }
    // Epoch interruption is enabled engine-wide, so a store that sets NO deadline would otherwise
    // trap immediately (its default deadline is the current epoch). Set it far out when there is
    // no deadline, so only fuel governs the run.
    let ticks = match limits.deadline {
        Some(deadline) => (deadline.after.as_millis() as u64).max(1),
        None => u64::MAX,
    };
    store.set_epoch_deadline(ticks);
}

/// The fuel a run consumed, given the fuel it started with; wall time is filled by the caller.
pub(crate) fn consumed_of<T>(store: &Store<T>, before: Option<u64>) -> Consumed {
    let after = store.get_fuel().ok();
    let fuel = match (before, after) {
        (Some(b), Some(a)) => Some(b.saturating_sub(a)),
        _ => None,
    };
    Consumed { fuel, wall: None }
}

/// Map a run failure to a typed error (R-D10). A denied memory growth is `OverBudget`; otherwise
/// a fuel trap is `OverBudget`, an epoch trap is `OverBudget` for a budget deadline and `Timeout`
/// for a host one, and any other trap is `Endpoint`. A host function that failed with a typed
/// kernel error (an unbound import, a refused binding) keeps its type.
pub(crate) fn trap_to_error(err: wasmtime::Error, limits: &Limits, memory_denied: bool) -> Error {
    if memory_denied {
        return Error::OverBudget {
            detail: "module exceeded its memory budget".to_string(),
        };
    }
    if let Some(trap) = err.downcast_ref::<Trap>() {
        return match trap {
            Trap::OutOfFuel => Error::OverBudget {
                detail: format!("module ran out of fuel (budget {:?})", limits.fuel),
            },
            Trap::Interrupt => match limits.deadline.map(|d| d.provenance) {
                Some(Provenance::Budget) => Error::OverBudget {
                    detail: "module ran past its time budget".to_string(),
                },
                _ => Error::Timeout("module ran past its deadline".to_string()),
            },
            other => Error::Endpoint(format!("module trapped: {other}")),
        };
    }
    if let Some(typed) = err.downcast_ref::<HostRefusal>() {
        return typed.0.clone();
    }
    Error::Endpoint(format!("module failed: {err:#}"))
}

/// A typed kernel error raised from inside a host function (an unbound import called, a WASI
/// interface no grant linked), carried through wasmtime's error so [`trap_to_error`] can return
/// it with its type intact.
#[derive(Debug)]
pub(crate) struct HostRefusal(pub(crate) Error);

impl std::fmt::Display for HostRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for HostRefusal {}

/// Build a `wasmtime::Error` from any `Display` message.
pub(crate) fn werr(message: impl std::fmt::Display) -> wasmtime::Error {
    wasmtime::Error::msg(message.to_string())
}
