# Drain error recovery contract

This document defines the error-recovery contract for the engine's drain paths
(parallel-delete, parallel-receive-delta, reorder buffer, channel-shutdown
sequences). It consolidates four pillars surfaced by recent audits and pins
the templates new code must follow.

Companion docs:

- `docs/audits/arc-try-unwrap-classification.md` (ATU-1 + ATU-2)
- `docs/audits/mutex-poison-policy.md` (MPE-1 + MPE-2)
- `docs/architecture/mutex-poison-policy.md` (MPE-11, planned)

## The four pillars

1. **Channel shutdown over `Arc::try_unwrap` where possible.** Prefer explicit
   producer-side EOF signalling (mpsc close, sentinel value, `tokio::sync::watch`
   shutdown flag) over `Arc::try_unwrap` for resource handoff. The unwrap
   variant only succeeds when no clone leaks - tests, panics in worker
   threads, or any forgotten `Arc::clone` site silently break it.
2. **Typed errors carrying `strong_count` + kind.** When `Arc::try_unwrap` is
   the right shape (lifetime is genuinely single-owner), wrap the failure in
   a typed error that captures `Arc::strong_count(&arc)` and an identifying
   tag. Never return bare `io::ErrorKind::Other`.
3. **Diagnostic logging on every drain failure.** Log `strong_count`,
   `weak_count`, and the call site as a `tracing::warn!` before returning the
   typed error. Operators need the count to triage which subsystem held the
   extra clone.
4. **No bare `io::ErrorKind::Other` returns.** Every error path inside the
   drain must surface a typed variant implementing `std::error::Error` with a
   `source()` chain. Callers can then downcast to react.

## Decision tree: "I have an `Arc<T>` that needs to be unwrapped"

```text
                         +-----------------+
                         | Need owned T?   |
                         +--------+--------+
                                  |
                +-----------------+----------------+
                | yes                              | no - keep Arc, exit
                v                                  v
       +--------+--------+
       | Single producer |
       | + N consumers?  |
       +--------+--------+
                |
       +--------+--------+
       | yes             | no - multiple producers, redesign
       v                 v
   Channel shutdown    Cannot use try_unwrap;
   (preferred)         add explicit barrier
       |
       v
   `try_unwrap()` is now safe (consumers all dropped on EOF)
   - typed error variant if it still fails
   - log strong_count
```

## Typed-error template

```rust
#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    #[error("{kind} still referenced by {strong_count} holder(s)")]
    ResourceStillReferenced {
        strong_count: usize,
        kind: &'static str,
    },
    #[error("drain channel closed before EOF")]
    PrematureShutdown,
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl DrainError {
    pub fn from_failed_unwrap<T>(arc: Arc<T>, kind: &'static str) -> Self {
        DrainError::ResourceStillReferenced {
            strong_count: Arc::strong_count(&arc),
            kind,
        }
    }
}
```

Use site:

```rust
let owned = Arc::try_unwrap(plan_map_arc)
    .map_err(|arc| DrainError::from_failed_unwrap(arc, "DeletePlanMap"))?;
```

## Diagnostic-logging template

Log before returning. Operators reading the log see why the drain
failed, not just "Other".

```rust
let owned = match Arc::try_unwrap(plan_map_arc) {
    Ok(v) => v,
    Err(arc) => {
        tracing::warn!(
            strong_count = Arc::strong_count(&arc),
            weak_count = Arc::weak_count(&arc),
            kind = "DeletePlanMap",
            "Arc::try_unwrap failed during drain - extra clones outlived the producer"
        );
        return Err(DrainError::from_failed_unwrap(arc, "DeletePlanMap"));
    }
};
```

## No-bare-Other template

Audit any `io::Error::new(io::ErrorKind::Other, msg)` in error-conversion
code. Replace with a typed variant that implements `std::error::Error` and
keeps a `source()` chain:

```rust
// BAD
return Err(io::Error::new(io::ErrorKind::Other, "drain failed"));

// GOOD
return Err(MyOpError::DrainFailed { cause: source_err }.into());
```

The wrapper can still `impl From<MyOpError> for io::Error` to preserve the
callsite signature, but the typed error stays downcast-recoverable.

## Cross-reference to Mutex poison policy

`Mutex::lock().expect("poisoned")` shares the same anti-pattern: panic in one
thread propagates as a panic in every subsequent lock attempt. The MPE series
(`lock_or_recover` helper, `docs/audits/mutex-poison-policy.md`) gives the
recovery primitives. Drain code that touches a `Mutex` should use
`lock_or_recover` AND surface drain failures as typed errors per this
contract.

## Worked example: `DeleteContext::into_emitter`

Before (per `docs/audits/arc-try-unwrap-classification.md` top fragile site):

```rust
let plan_map = Arc::try_unwrap(self.plan_map)
    .map_err(|_| io::Error::new(io::ErrorKind::Other, "plan map still shared"))?;
```

After (the shape ATU-3 / PR pending implements):

```rust
let plan_map = Arc::try_unwrap(self.plan_map).map_err(|arc| {
    let strong_count = Arc::strong_count(&arc);
    tracing::warn!(strong_count, kind = "DeletePlanMap",
        "DeleteContext::into_emitter could not unwrap plan map");
    DeleteError::PlanMapStillShared { strong_count }
})?;
```

The caller in `local_copy::executor::cleanup::delete_extraneous_entries_via_emitter`
now sees a typed `DeleteError::PlanMapStillShared { strong_count: 2 }` instead
of an opaque `io::ErrorKind::Other`, and the warn line gives the operator the
count without enabling debug-level logging.

## Promotion path

1. ATU-3 lands the typed error variants for the 3 production fragile sites.
2. ATU-4 refactors the most fragile site (drain path) to channel-shutdown.
3. ATU-5 lands the diagnostic logging template across the remaining sites.
4. ATU-6 replaces every `io::ErrorKind::Other` in error-conversion code.
5. ATU-7 adds the stress test asserting typed errors surface, not `Other`.
6. This contract becomes a CI lint: any new `Arc::try_unwrap` outside
   `#[cfg(test)]` without a typed error mapping fails review.
