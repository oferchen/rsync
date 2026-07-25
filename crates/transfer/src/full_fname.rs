//! Quoted path rendering for diagnostic messages.
//!
//! Upstream rsync funnels every path that appears inside an error or warning
//! through `full_fname()`, which wraps the name in double quotes and, when the
//! process is serving a daemon module, appends ` (in MODULE)` *after* the
//! closing quote. A daemon-side open failure therefore reads:
//!
//! ```text
//! rsync: [sender] send_files failed to open "sub/denied.txt" (in mymod): Permission denied (13)
//! ```
//!
//! Only the message sites that upstream routes through `full_fname()` carry the
//! suffix. Sites that hard-code the quotes around a plain `%s` (for example
//! `copying unsafe symlink "%s" -> "%s"` at `flist.c:229`, or
//! `not creating new %s "%s"` at `generator.c:1380`) never gain it, so they must
//! keep formatting their own quotes.
//!
//! # Upstream Reference
//!
//! - `util1.c:1273` - `full_fname()`; the `module_id >= 0` branch selects
//!   `" (in "`, `lp_name(module_id)`, `")"`.
//! - `clientserver.c:769` - `module_id = i` is the only assignment that makes
//!   `module_id >= 0`, so the suffix appears exactly when the process is a
//!   daemon server that has selected a module.

use std::fmt::Write as _;
use std::path::Path;

/// Renders `fname` the way upstream `full_fname()` does: double quoted, with
/// ` (in MODULE)` appended after the closing quote when serving a daemon
/// module.
///
/// `module` is `None` for client and non-daemon server processes, mirroring
/// upstream's `module_id < 0`.
///
/// Upstream additionally rewrites a relative name into `curr_dir` +
/// `module_dirlen` form. Inside a daemon module that prefix is always empty,
/// which is the only context where the suffix applies, so this helper renders
/// the name it is given.
///
/// # Upstream Reference
///
/// - `util1.c:1296` - `asprintf(&result, "\"%s%s%s\"%s%s%s", ...)`
pub(crate) fn full_fname(fname: &str, module: Option<&str>) -> String {
    let mut out = String::with_capacity(fname.len() + 2);
    out.push('"');
    out.push_str(fname);
    out.push('"');
    if let Some(module) = module {
        let _ = write!(out, " (in {module})");
    }
    out
}

/// [`full_fname`] for a [`Path`], using the platform's lossy display form.
pub(crate) fn full_fname_path(path: &Path, module: Option<&str>) -> String {
    full_fname(&path.display().to_string(), module)
}

#[cfg(test)]
mod tests {
    use super::{full_fname, full_fname_path};
    use std::path::Path;

    #[test]
    fn quotes_without_module_outside_daemon() {
        assert_eq!(full_fname("sub/denied.txt", None), "\"sub/denied.txt\"");
    }

    #[test]
    fn appends_module_suffix_after_closing_quote() {
        // Ground truth captured from upstream rsync 3.4.4 serving module
        // `mymod`: `send_files failed to open "sub/denied.txt" (in mymod)`.
        assert_eq!(
            full_fname("sub/denied.txt", Some("mymod")),
            "\"sub/denied.txt\" (in mymod)"
        );
    }

    #[test]
    fn empty_module_name_still_renders_upstream_shape() {
        // upstream: lp_name() can return an empty string only for a malformed
        // config; `module_id >= 0` still selects the suffix branch.
        assert_eq!(full_fname("f", Some("")), "\"f\" (in )");
    }

    #[test]
    fn path_variant_matches_string_variant() {
        assert_eq!(
            full_fname_path(Path::new("a/b"), Some("mod")),
            full_fname("a/b", Some("mod"))
        );
    }
}
