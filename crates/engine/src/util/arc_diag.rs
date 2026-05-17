//! Diagnostic helpers for `Arc` ownership transfer failures.
//!
//! Drain paths that need exclusive ownership of an `Arc<T>` (delete plan
//! map, parallel-apply file slot) call [`Arc::try_unwrap`] when handing the
//! inner value off to a single owner. When another caller still holds a
//! clone, the unwrap returns the residual `Arc` and the typed error fires.
//! Without context the failure looks like a plain "still shared" message.
//!
//! [`try_unwrap_or_log`] wraps the unwrap so the residual reference counts
//! and a callsite tag are emitted via `tracing::warn!` before the error is
//! returned. When the `tracing` feature is disabled the helper still
//! returns the same `Result`, but the log emission becomes a no-op.

use std::sync::Arc;

/// Tries to move the inner value out of `arc`. On failure, logs the residual
/// strong and weak reference counts together with `kind` before returning
/// the original `Arc` to the caller.
///
/// `kind` is a `'static` tag identifying the callsite (for example,
/// `"delete::context::plans"` or `"parallel_apply::file_slot"`).
///
/// # Examples
///
/// ```
/// use engine::util::try_unwrap_or_log;
/// use std::sync::Arc;
///
/// let owned = Arc::new(42);
/// let value = try_unwrap_or_log(owned, "doctest::sole_owner").expect("sole owner");
/// assert_eq!(value, 42);
///
/// let shared = Arc::new(1u32);
/// let clone = Arc::clone(&shared);
/// let residual = try_unwrap_or_log(shared, "doctest::still_shared")
///     .expect_err("still shared");
/// assert_eq!(*residual, 1);
/// drop(clone);
/// ```
#[inline]
pub fn try_unwrap_or_log<T>(arc: Arc<T>, kind: &'static str) -> Result<T, Arc<T>> {
    Arc::try_unwrap(arc).inspect_err(|residual| log_residual(residual, kind))
}

#[cfg(feature = "tracing")]
#[inline]
fn log_residual<T>(residual: &Arc<T>, kind: &'static str) {
    tracing::warn!(
        strong_count = Arc::strong_count(residual),
        weak_count = Arc::weak_count(residual),
        kind = kind,
        "Arc::try_unwrap failed"
    );
}

#[cfg(not(feature = "tracing"))]
#[inline]
fn log_residual<T>(_residual: &Arc<T>, _kind: &'static str) {}

#[cfg(test)]
mod tests {
    use super::try_unwrap_or_log;
    use std::sync::Arc;

    #[test]
    fn returns_inner_when_sole_owner() {
        let arc = Arc::new(42u32);
        let value = try_unwrap_or_log(arc, "test::sole").expect("sole owner");
        assert_eq!(value, 42);
    }

    #[test]
    fn returns_original_arc_when_still_shared() {
        let arc = Arc::new(7u32);
        let leaked = Arc::clone(&arc);
        let residual = try_unwrap_or_log(arc, "test::still_shared")
            .expect_err("expected residual Arc on shared input");
        assert_eq!(*residual, 7);
        assert_eq!(Arc::strong_count(&residual), 2);
        drop(leaked);
        drop(residual);
    }

    #[test]
    fn weak_reference_blocks_unwrap_and_is_reported() {
        let arc = Arc::new(99u32);
        let weak = Arc::downgrade(&arc);
        // Strong count is 1 but the weak ref keeps try_unwrap from succeeding.
        let residual =
            try_unwrap_or_log(arc, "test::weak_holder").expect_err("weak ref must block unwrap");
        assert_eq!(*residual, 99);
        assert_eq!(Arc::weak_count(&residual), 1);
        drop(weak);
        drop(residual);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn warn_event_emitted_on_failure() {
        use std::sync::Mutex;
        use tracing::subscriber::with_default;
        use tracing::{Event, Level, Subscriber};

        // Shared sink survives subscriber move into `with_default`.
        static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

        struct Capture;

        impl Subscriber for Capture {
            fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
                metadata.level() <= &Level::WARN
            }
            fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {
            }
            fn event(&self, event: &Event<'_>) {
                struct V<'a>(&'a mut String);
                impl tracing::field::Visit for V<'_> {
                    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                        use std::fmt::Write;
                        let _ = write!(self.0, " {}={}", field.name(), value);
                    }
                    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                        use std::fmt::Write;
                        let _ = write!(self.0, " {}={}", field.name(), value);
                    }
                    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
                        use std::fmt::Write;
                        let _ = write!(self.0, " {}={}", field.name(), value);
                    }
                    fn record_debug(
                        &mut self,
                        field: &tracing::field::Field,
                        value: &dyn std::fmt::Debug,
                    ) {
                        use std::fmt::Write;
                        let _ = write!(self.0, " {}={:?}", field.name(), value);
                    }
                }
                let mut line = format!("{}", event.metadata().level());
                event.record(&mut V(&mut line));
                EVENTS.lock().expect("capture mutex poisoned").push(line);
            }
            fn enter(&self, _span: &tracing::span::Id) {}
            fn exit(&self, _span: &tracing::span::Id) {}
        }

        EVENTS.lock().expect("capture mutex poisoned").clear();

        with_default(Capture, || {
            let arc = Arc::new(1u8);
            let clone = Arc::clone(&arc);
            let _err = try_unwrap_or_log(arc, "test::warn_event").expect_err("shared");
            drop(clone);
        });

        let events = EVENTS.lock().expect("capture mutex poisoned");
        assert!(
            events
                .iter()
                .any(|line| line.contains("Arc::try_unwrap failed")
                    && line.contains("test::warn_event")
                    && line.contains("strong_count=2")),
            "expected warn event with diagnostics, got {events:?}",
        );
    }
}
