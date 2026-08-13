//! Upstream `clean_fname()` `..`-collapse cases, shared by every oc-rsync
//! module that implements the same rule.
//!
//! rsync 3.5.0 fixed an off-by-one in `clean_fname()`'s `..`-collapse: after
//! walking the write pointer back over the previous component, the boundary
//! test read `*s` instead of `s[-1]` and the truncation point was `s + 1`
//! instead of `s`. The effect was that the collapse silently did nothing for
//! every multi-component and absolute path - `a/b/../c` stayed `a/b/../c`.
//! Upstream shipped `t_clean_fname.c` as the unit harness that pins the fix.
//!
//! oc-rsync has no single `clean_fname`; the same rule is implemented
//! independently in several places (alt-basis resolution in `engine`, daemon
//! basis confinement, `sanitize_path` in `transfer`). Keeping the case table
//! here means a new edge case is one row and is checked against every
//! implementation at once, rather than fixed in whichever copy happened to be
//! reported.
//!
//! # Upstream References
//!
//! - `util1.c` - `clean_fname()`, the `CFN_COLLAPSE_DOT_DOT_DIRS` branch.
//! - `t_clean_fname.c` - upstream's unit harness; this table is its `cases[]`.
//! - `testsuite/clean-fname-collapse_test.py` - the testsuite entry that runs it.

/// Input / expected-output pairs for upstream
/// `clean_fname(name, CFN_COLLAPSE_DOT_DOT_DIRS)`.
///
/// Transcribed verbatim from the `cases[]` array in upstream's own
/// `t_clean_fname.c` (rsync 3.5.0), so the expectations are upstream's, not a
/// re-derivation. Every entry exercises a `..` that must consume the component
/// before it: interior (`a/b/../c`), absolute (`/x/y/../z`), single-component
/// (`a/../b`), consecutive (`p/q/r/../../s`), and trailing (`d/e/..`).
///
/// Only `a/../b` collapsed correctly before the 3.5.0 fix - its backward walk
/// stops at the buffer start, which took the one branch arm the off-by-one did
/// not corrupt. The other four are the regression cases.
///
/// # Upstream Reference
///
/// - `t_clean_fname.c` - `cases[]`.
pub const COLLAPSE_CASES: &[(&str, &str)] = &[
    ("a/b/../c", "a/c"),
    ("/x/y/../z", "/x/z"),
    ("a/../b", "b"),
    ("p/q/r/../../s", "p/s"),
    ("d/e/..", "d"),
];
