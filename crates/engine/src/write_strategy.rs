//! Which resolved options force the inplace write strategy.
//!
//! This crate owns the temp-file-write-plus-atomic-rename strategy that
//! `--inplace` bypasses, so the rule deciding when that bypass is mandatory
//! lives here rather than being re-derived by each front end.
//!
//! Upstream resolves it once in `parse_arguments()` and every peer runs that
//! same function, so client and server always agree. oc reaches the strategy
//! from two independent places - the local-copy/client config built in `cli`
//! and the `ServerConfig` built in `transfer` - which is how `--write-devices`
//! came to be honoured on one and not the other. Both now call
//! [`implies_inplace`], so a future option cannot be added to one and forgotten
//! in the other.

/// Whether the resolved options force `inplace` on.
///
/// Upstream evaluates two adjacent blocks, each setting the same global; this
/// is that pair as a single expression.
///
/// # Upstream Reference
///
/// - `options.c:2400-2411` - `if (append_mode) { ...; inplace = 1; }`.
/// - `options.c:2413-2419` - `if (write_devices) { ...; inplace = 1; }`.
#[must_use]
pub const fn implies_inplace(inplace: bool, append: bool, write_devices: bool) -> bool {
    inplace || append || write_devices
}

#[cfg(test)]
mod tests {
    use super::implies_inplace;

    /// The full truth table, so the rule is pinned rather than inferred from
    /// whichever call site a reader happens to open first.
    #[test]
    fn every_input_combination_matches_upstream() {
        for (inplace, append, write_devices, expected) in [
            (false, false, false, false),
            (false, false, true, true),
            (false, true, false, true),
            (false, true, true, true),
            (true, false, false, true),
            (true, false, true, true),
            (true, true, false, true),
            (true, true, true, true),
        ] {
            assert_eq!(
                implies_inplace(inplace, append, write_devices),
                expected,
                "inplace={inplace} append={append} write_devices={write_devices}",
            );
        }
    }

    /// `--write-devices` alone must promote. This is the case that regressed:
    /// the server-side promotion covered only `--append`, so a server given
    /// `--write-devices` kept temp+rename where upstream writes in place.
    ///
    /// upstream: `options.c:2413-2419`.
    #[test]
    fn write_devices_alone_promotes() {
        assert!(implies_inplace(false, false, true));
    }

    /// `--append` alone must promote, the sibling rule that was already
    /// handled - pinned here so collapsing the two did not drop it.
    ///
    /// upstream: `options.c:2400-2411`.
    #[test]
    fn append_alone_promotes() {
        assert!(implies_inplace(false, true, false));
    }

    /// No option set means no promotion: the default stays temp+rename.
    #[test]
    fn nothing_set_leaves_temp_plus_rename() {
        assert!(!implies_inplace(false, false, false));
    }
}
