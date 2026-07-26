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
//! # Module-relative rendering
//!
//! A daemon server `chdir()`s into the module root (`clientserver.c:993`
//! `change_dir(module_chdir, CD_NORMAL)`), so every path it later handles is
//! *relative* to that root and the absolute server-side location never reaches
//! the client. `full_fname()` re-attaches only the part of `curr_dir` that lies
//! below the module root:
//!
//! ```c
//! if (*fn == '/')
//!         p1 = p2 = "";
//! else {
//!         p1 = curr_dir + module_dirlen;
//!         for (p2 = p1; *p2 == '/'; p2++) {}
//!         if (*p2)
//!                 p2 = "/";
//! }
//! ```
//!
//! With `curr_dir` at the module root `p1` is empty and the rendered name is
//! the bare relative path (`"denied"`); with `curr_dir` one level down `p1` is
//! `/sub`, `p2` collapses to `/`, and the render is module-root anchored
//! (`"/sub/denied2"`). Both forms were captured from rsync 3.4.4 serving a
//! module; neither ever contains the daemon's real filesystem prefix.
//!
//! oc-rsync never `chdir()`s - it carries absolute paths throughout - so
//! [`DaemonPaths`] supplies the two directories upstream keeps in globals and
//! the helper recovers upstream's relative `fn` by stripping `curr_dir`.
//!
//! # Upstream Reference
//!
//! - `util1.c:1273` - `full_fname()`; the `module_id >= 0` branch selects
//!   `" (in "`, `lp_name(module_id)`, `")"`.
//! - `clientserver.c:769` - `module_id = i` is the only assignment that makes
//!   `module_id >= 0`, so the suffix appears exactly when the process is a
//!   daemon server that has selected a module.
//! - `clientserver.c:864,993` - `module_dirlen` is the length of the
//!   normalized module path and the server `chdir()`s there before serving.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The daemon path context upstream keeps in globals, as seen by
/// [`full_fname`].
///
/// `module` is upstream's `lp_name(module_id)`, `module_root` is `module_dir`
/// (whose length is `module_dirlen`), and `curr_dir` is the directory upstream
/// has `chdir()`ed into - the receiver's destination, or the sender's per-arg
/// `dir` from the `flist.c:2338-2349` split. Absent for client and non-daemon
/// server processes, mirroring upstream's `module_id < 0`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DaemonPaths<'a> {
    /// Module name appended as ` (in MODULE)`. upstream: `lp_name(module_id)`.
    pub module: &'a str,
    /// Absolute module root. upstream: `module_dir` / `module_dirlen`.
    pub module_root: &'a Path,
    /// Absolute directory the server is serving from. upstream: `curr_dir`.
    pub curr_dir: &'a Path,
}

impl DaemonPaths<'_> {
    /// Rewrites an absolute server-side path into the module-relative form
    /// upstream renders, or returns `None` when the path lies outside the
    /// served tree.
    ///
    /// `None` makes the caller fall back to the path as given, matching
    /// upstream's `*fn == '/'` branch: an absolute `fn` gets no prefix and is
    /// printed verbatim.
    fn relativize(&self, path: &Path) -> Option<PathBuf> {
        // upstream's `fn` is relative to `curr_dir`, so recover it first.
        let tail = path.strip_prefix(self.curr_dir).ok()?;
        // upstream: `p1 = curr_dir + module_dirlen` - the part of the working
        // directory below the module root.
        let p1 = self.curr_dir.strip_prefix(self.module_root).ok()?;
        // A DOTDIR source arg leaves `fn` as ".", never empty.
        let tail = if tail.as_os_str().is_empty() {
            Path::new(".")
        } else {
            tail
        };
        if p1.as_os_str().is_empty() {
            // upstream: p1 == "" and p2 == "" - the bare relative name.
            return Some(tail.to_path_buf());
        }
        // upstream: p1 == "/sub" and p2 == "/" - module-root anchored.
        Some(Path::new("/").join(p1).join(tail))
    }
}

/// Renders `fname` the way upstream `full_fname()` does: double quoted,
/// rewritten relative to the daemon module root, and with ` (in MODULE)`
/// appended after the closing quote when serving a daemon module.
///
/// # Upstream Reference
///
/// - `util1.c:1296` - `asprintf(&result, "\"%s%s%s\"%s%s%s", ...)`
pub(crate) fn full_fname(fname: &str, daemon: Option<DaemonPaths<'_>>) -> String {
    match daemon {
        Some(paths) => match paths.relativize(Path::new(fname)) {
            Some(rendered) => quote(&rendered.display().to_string(), Some(paths.module)),
            None => quote(fname, Some(paths.module)),
        },
        None => quote(fname, None),
    }
}

/// [`full_fname`] for a [`Path`], using the platform's lossy display form.
pub(crate) fn full_fname_path(path: &Path, daemon: Option<DaemonPaths<'_>>) -> String {
    match daemon {
        Some(paths) => {
            let rendered = paths.relativize(path);
            let shown = rendered.as_deref().unwrap_or(path);
            quote(&shown.display().to_string(), Some(paths.module))
        }
        None => quote(&path.display().to_string(), None),
    }
}

/// Formats the quoted name plus the optional ` (in MODULE)` suffix.
fn quote(fname: &str, module: Option<&str>) -> String {
    let mut out = String::with_capacity(fname.len() + 2);
    out.push('"');
    out.push_str(fname);
    out.push('"');
    if let Some(module) = module {
        let _ = write!(out, " (in {module})");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{DaemonPaths, full_fname, full_fname_path};
    use std::path::Path;

    fn paths<'a>(module_root: &'a str, curr_dir: &'a str) -> DaemonPaths<'a> {
        DaemonPaths {
            module: "mymod",
            module_root: Path::new(module_root),
            curr_dir: Path::new(curr_dir),
        }
    }

    #[test]
    fn quotes_without_module_outside_daemon() {
        assert_eq!(full_fname("sub/denied.txt", None), "\"sub/denied.txt\"");
    }

    #[test]
    fn non_daemon_path_is_left_absolute() {
        // A client or SSH server process has `module_id < 0`: upstream neither
        // strips a prefix nor appends a suffix, so the absolute path a local
        // or SSH run reports must stay byte-identical.
        assert_eq!(
            full_fname_path(Path::new("/tmp/src/denied.txt"), None),
            "\"/tmp/src/denied.txt\""
        );
    }

    #[test]
    fn appends_module_suffix_after_closing_quote() {
        // Ground truth captured from upstream rsync 3.4.4 serving module
        // `mymod`: `send_files failed to open "sub/denied.txt" (in mymod)`.
        assert_eq!(
            full_fname_path(
                Path::new("/srv/mod/sub/denied.txt"),
                Some(paths("/srv/mod", "/srv/mod"))
            ),
            "\"sub/denied.txt\" (in mymod)"
        );
    }

    #[test]
    fn curr_dir_below_module_root_anchors_with_a_leading_slash() {
        // Ground truth, rsync 3.4.4 daemon, `rsync -r rsync://h:p/mod/sub/`:
        //   rsync: [sender] opendir "/sub/denied2" (in mod) failed: ...
        // upstream: p1 = curr_dir + module_dirlen = "/sub", p2 = "/".
        assert_eq!(
            full_fname_path(
                Path::new("/srv/mod/sub/denied2"),
                Some(paths("/srv/mod", "/srv/mod/sub"))
            ),
            "\"/sub/denied2\" (in mymod)"
        );
    }

    #[test]
    fn path_at_curr_dir_renders_as_dot() {
        // upstream's DOTDIR_NAME arg leaves `fn` as ".", never the empty
        // string (flist.c:2312-2322).
        assert_eq!(
            full_fname_path(Path::new("/srv/mod"), Some(paths("/srv/mod", "/srv/mod"))),
            "\".\" (in mymod)"
        );
    }

    #[test]
    fn path_outside_the_served_tree_stays_verbatim() {
        // upstream: `*fn == '/'` selects `p1 = p2 = ""`, so an absolute name
        // is printed as-is with the module suffix still attached.
        assert_eq!(
            full_fname_path(
                Path::new("/etc/passwd"),
                Some(paths("/srv/mod", "/srv/mod"))
            ),
            "\"/etc/passwd\" (in mymod)"
        );
    }

    #[test]
    fn empty_module_name_still_renders_upstream_shape() {
        // upstream: lp_name() can return an empty string only for a malformed
        // config; `module_id >= 0` still selects the suffix branch.
        assert_eq!(
            full_fname(
                "/srv/mod/f",
                Some(DaemonPaths {
                    module: "",
                    module_root: Path::new("/srv/mod"),
                    curr_dir: Path::new("/srv/mod"),
                })
            ),
            "\"f\" (in )"
        );
    }

    #[test]
    fn path_variant_matches_string_variant() {
        assert_eq!(
            full_fname_path(
                Path::new("/srv/mod/a/b"),
                Some(paths("/srv/mod", "/srv/mod"))
            ),
            full_fname("/srv/mod/a/b", Some(paths("/srv/mod", "/srv/mod")))
        );
    }
}
