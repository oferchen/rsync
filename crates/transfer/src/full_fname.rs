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
//! # The two independent axes
//!
//! `full_fname()` makes **two** decisions, and they are gated on two different
//! globals:
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
//! if (module_id >= 0) {
//!         m1 = " (in "; m2 = lp_name(module_id); m3 = ")";
//! } else
//!         m1 = m2 = m3 = "";
//! ```
//!
//! The **prefix** `p1 + p2` is computed unconditionally: every process has a
//! `curr_dir`, and `module_dirlen` is simply `0` when there is no module to
//! strip (`clientserver.c:106` initialises it to `0`). Only the **suffix** is
//! conditional on `module_id >= 0`. So a plain local or SSH run still prefixes
//! its working directory, which is why upstream reports an absolute name for a
//! relative operand:
//!
//! ```text
//! $ cd /tmp/work && rsync -r nope dst/
//! rsync: [sender] link_stat "/tmp/work/nope" failed: No such file or directory (2)
//! ```
//!
//! [`FullFnamePaths`] keeps the two axes separate for exactly that reason:
//! [`module`](FullFnamePaths::module) is the suffix axis and is `None` outside a
//! daemon module, while [`module_root`](FullFnamePaths::module_root) is the
//! strip axis and is `None` whenever `module_dirlen` would be `0`.
//!
//! # Module-relative rendering
//!
//! A daemon server `chdir()`s into the module root (`clientserver.c:1059`
//! `change_dir(module_chdir, CD_NORMAL)`), so every path it later handles is
//! *relative* to that root and the absolute server-side location never reaches
//! the client. With `curr_dir` at the module root `p1` is empty and the
//! rendered name is the bare relative path (`"denied"`); with `curr_dir` one
//! level down `p1` is `/sub`, `p2` collapses to `/`, and the render is
//! module-root anchored (`"/sub/denied2"`). Both forms were captured from
//! rsync 3.4.4 serving a module; neither ever contains the daemon's real
//! filesystem prefix.
//!
//! A module whose `path` is `/` is the exception, and it is upstream's own:
//! `clientserver.c:922-923` forces `module_dirlen` back to `0`, so nothing is
//! stripped and the absolute name is what upstream prints.
//!
//! oc-rsync never `chdir()`s - it carries the operand spelling it was given -
//! so [`FullFnamePaths`] supplies the directories upstream keeps in globals and
//! the helper recovers upstream's `curr_dir`-relative `fn` before re-attaching
//! the prefix.
//!
//! # Upstream Reference
//!
//! - `util1.c:1433` - `full_fname()`; the `module_id >= 0` branch selects
//!   `" (in "`, `lp_name(module_id)`, `")"`.
//! - `util1.c:1445-1452` - the `*fn == '/'` test and `p1 = curr_dir +
//!   module_dirlen` (`util1.c:1448`), computed with no reference to
//!   `module_id`.
//! - `clientserver.c:821` - `module_id = i` is the only assignment that makes
//!   `module_id >= 0`, so the suffix appears exactly when the process is a
//!   daemon server that has selected a module.
//! - `clientserver.c:106` - `unsigned int module_dirlen = 0;` - the strip
//!   length outside a daemon module.
//! - `clientserver.c:864,993` - inside a module `module_dirlen` is the length
//!   of the normalized module path, and the server `chdir()`s there before
//!   serving.
//! - `clientserver.c:922-923` - `if (module_dirlen == 1) module_dirlen = 0;` -
//!   a module rooted at `/` strips nothing, so `p1` keeps `curr_dir`'s leading
//!   slash and the rendered name stays absolute.
//! - `util1.c:1224` - `getcwd(curr_dir, ...)` seeds the working directory once.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// The path context upstream keeps in globals, as seen by [`full_fname`].
///
/// The three fields are upstream's three independent pieces of state, and each
/// is conditional on its own terms:
///
/// - `module` is `lp_name(module_id)`, present exactly when `module_id >= 0`.
/// - `module_root` is `module_dir`, whose length is `module_dirlen`; `None`
///   expresses `module_dirlen == 0`.
/// - `curr_dir` is the directory names are rendered against, and upstream
///   always has one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FullFnamePaths<'a> {
    /// Module name appended as ` (in MODULE)`. upstream: `lp_name(module_id)`,
    /// selected by `module_id >= 0`. `None` for clients and for non-daemon
    /// (SSH) server processes, mirroring upstream's `module_id < 0`.
    pub module: Option<&'a str>,
    /// Prefix stripped from `curr_dir` before rendering. upstream:
    /// `module_dir` / `module_dirlen`.
    ///
    /// `None` is upstream's `module_dirlen == 0`, which covers three cases:
    /// a process serving no module at all (`clientserver.c:106`), a module
    /// whose `path` is `/` (`clientserver.c:922-923`), and a chrooted module,
    /// whose root becomes `/` inside the jail (`clientserver.c:912-913`).
    pub module_root: Option<&'a Path>,
    /// Absolute directory names are rendered against. upstream: `curr_dir` -
    /// the receiver's destination, the sender's per-arg `dir` from the
    /// `flist.c:2608-2637` split, or the process working directory.
    pub curr_dir: &'a Path,
}

impl<'a> FullFnamePaths<'a> {
    /// The context a process that is not serving a daemon module renders
    /// against: no suffix, nothing stripped, and the process working directory
    /// as `curr_dir`.
    #[must_use]
    pub(crate) fn non_daemon() -> Self {
        Self {
            module: None,
            module_root: None,
            curr_dir: process_curr_dir(),
        }
    }

    /// The context a daemon server renders against.
    ///
    /// `module_root` is dropped when it is `/`, because upstream forces
    /// `module_dirlen` to `0` for that module (`clientserver.c:922-923`) and
    /// then prints the absolute name.
    #[must_use]
    pub(crate) fn daemon(module: &'a str, module_root: &'a Path, curr_dir: &'a Path) -> Self {
        Self {
            module: Some(module),
            module_root: strip_prefix_root(module_root),
            curr_dir,
        }
    }

    /// Rewrites a server-side path into the form upstream renders, or returns
    /// `None` when the path lies outside the tree `curr_dir` anchors.
    ///
    /// `None` makes the caller fall back to the path as given, matching
    /// upstream's `*fn == '/'` branch: an absolute `fn` gets no prefix and is
    /// printed verbatim.
    fn render(&self, path: &Path) -> Option<String> {
        // upstream's `fn` is relative to `curr_dir`. oc-rsync hands this
        // helper either an absolute path (the daemon and every wire-side
        // caller) or the operand spelling the user typed, which is already
        // relative to the working directory - so recover `fn` from both.
        let tail = if path.is_absolute() {
            slash_path(path.strip_prefix(self.curr_dir).ok()?)
        } else {
            slash_path(path)
        };
        // A DOTDIR source arg leaves `fn` as ".", never empty
        // (`flist.c:2672-2673`).
        let tail = if tail.is_empty() {
            ".".to_owned()
        } else {
            tail
        };
        // upstream: `p1 = curr_dir + module_dirlen` - a byte offset into
        // `curr_dir`, so `module_dirlen == 0` leaves the whole of it.
        let p1 = match self.module_root {
            Some(root) => {
                let below = slash_path(self.curr_dir.strip_prefix(root).ok()?);
                if below.is_empty() {
                    String::new()
                } else {
                    format!("/{below}")
                }
            }
            None => slash_path(self.curr_dir),
        };
        // upstream: `for (p2 = p1; *p2 == '/'; p2++) {}` then
        // `if (*p2) p2 = "/";` - a separator only when something survives the
        // leading slashes.
        let p2 = if p1.trim_start_matches('/').is_empty() {
            ""
        } else {
            "/"
        };
        Some(format!("{p1}{p2}{tail}"))
    }
}

/// Upstream's `curr_dir[]` for a process that never selected a module.
///
/// Upstream seeds the global once with `getcwd()` (`util1.c:1224`) and moves it
/// with `change_dir()`. oc-rsync never `chdir()`s, so the process working
/// directory is that value for the whole run.
///
/// A failed `getcwd()` yields an empty path, which renders `p1` and `p2` empty
/// and prints the bare name - the same output as before this prefix existed,
/// rather than a panic inside a diagnostic.
fn process_curr_dir() -> &'static Path {
    static CWD: OnceLock<PathBuf> = OnceLock::new();
    CWD.get_or_init(|| std::env::current_dir().unwrap_or_default())
}

/// Maps a module root of `/` to "strip nothing".
///
/// upstream: `clientserver.c:922-923` - `if (module_dirlen == 1)
/// module_dirlen = 0;`. A module rooted at `/` has `module_dir == "/"`, whose
/// length is 1, so upstream deliberately consumes no byte of `curr_dir` and the
/// rendered name stays absolute.
fn strip_prefix_root(module_root: &Path) -> Option<&Path> {
    if module_root == Path::new("/") {
        None
    } else {
        Some(module_root)
    }
}

/// Renders a path with `/` separators, dropping `.` components.
///
/// The name upstream prints is the same `/`-separated name it puts on the wire,
/// never a host-native one, so the separator must not follow the platform the
/// process happens to run on. A leading root component contributes exactly one
/// `/`.
///
/// `.` components are dropped because upstream never carries one into a
/// diagnostic: the sender cleans every flist name with `clean_fname(thisname,
/// 0)` (`flist.c:1424`), and with `CFN_KEEP_DOT_DIRS` unset that discards
/// interior `"."` dirs (`util1.c:1068-1071`). An operand of `./sub/` therefore
/// prints as `<curr_dir>/sub/...`, not `<curr_dir>/./sub/...`.
///
/// A path that is nothing but `.` components renders empty; [`FullFnamePaths::render`]
/// turns that back into `"."`, which is upstream's DOTDIR name
/// (`flist.c:2672-2673`).
fn slash_path(path: &Path) -> String {
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::RootDir => out.push('/'),
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&other.as_os_str().to_string_lossy());
            }
        }
    }
    out
}

/// Renders `fname` the way upstream `full_fname()` does: double quoted,
/// prefixed with the part of `curr_dir` that survives `module_dirlen`, and with
/// ` (in MODULE)` appended after the closing quote when serving a daemon
/// module.
///
/// # Upstream Reference
///
/// - `util1.c:1460` - `asprintf(&result, "\"%s%s%s\"%s%s%s", ...)`
pub(crate) fn full_fname(fname: &str, paths: FullFnamePaths<'_>) -> String {
    match paths.render(Path::new(fname)) {
        Some(rendered) => quote(&rendered, paths.module),
        None => quote(fname, paths.module),
    }
}

/// [`full_fname`] for a [`Path`], using the platform's lossy display form.
pub(crate) fn full_fname_path(path: &Path, paths: FullFnamePaths<'_>) -> String {
    match paths.render(path) {
        Some(rendered) => quote(&rendered, paths.module),
        None => quote(&path.display().to_string(), paths.module),
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
    use super::{FullFnamePaths, full_fname, full_fname_path};
    use std::path::Path;

    fn paths<'a>(module_root: &'a str, curr_dir: &'a str) -> FullFnamePaths<'a> {
        FullFnamePaths::daemon("mymod", Path::new(module_root), Path::new(curr_dir))
    }

    /// A context with no module and an explicit `curr_dir`, so the non-daemon
    /// prefix rule can be asserted without depending on the test process's own
    /// working directory.
    fn no_module(curr_dir: &str) -> FullFnamePaths<'_> {
        FullFnamePaths {
            module: None,
            module_root: None,
            curr_dir: Path::new(curr_dir),
        }
    }

    #[test]
    fn appends_module_suffix_after_closing_quote() {
        // Ground truth captured from upstream rsync 3.4.4 serving module
        // `mymod`: `send_files failed to open "sub/denied.txt" (in mymod)`.
        assert_eq!(
            full_fname_path(
                Path::new("/srv/mod/sub/denied.txt"),
                paths("/srv/mod", "/srv/mod")
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
                paths("/srv/mod", "/srv/mod/sub")
            ),
            "\"/sub/denied2\" (in mymod)"
        );
    }

    #[test]
    fn path_at_curr_dir_renders_as_dot() {
        // upstream's DOTDIR_NAME arg leaves `fn` as ".", never the empty
        // string (flist.c:2672-2673).
        assert_eq!(
            full_fname_path(Path::new("/srv/mod"), paths("/srv/mod", "/srv/mod")),
            "\".\" (in mymod)"
        );
    }

    #[test]
    fn path_outside_the_served_tree_stays_verbatim() {
        // upstream: `*fn == '/'` selects `p1 = p2 = ""`, so an absolute name
        // is printed as-is with the module suffix still attached.
        assert_eq!(
            full_fname_path(Path::new("/etc/passwd"), paths("/srv/mod", "/srv/mod")),
            "\"/etc/passwd\" (in mymod)"
        );
    }

    /// Ground truth, MEASURED 2026-09-07 against rsync 3.5.0 serving a module
    /// declared `path = /`, pulled with `-r -R` for a name that does not exist:
    ///
    /// ```text
    /// rsync: [sender] link_stat "/tmp/t1141/msg/srv/mod/nope" (in root) failed: ...
    /// ```
    ///
    /// The `--relative` walk base is the module root, so `curr_dir` is `/` and
    /// upstream's `p1 = curr_dir + module_dirlen` is `/` - `module_dirlen` is
    /// forced to 0, not 1, at `clientserver.c:922-923`. Treating `/` as a path
    /// prefix instead of a byte count drops that slash and renders the name
    /// relative, which is what oc emitted before this pin.
    #[test]
    fn module_rooted_at_slash_keeps_the_leading_slash() {
        // The case #7728 pinned: a module rooted at `/` with `curr_dir` also
        // `/`. `module_dirlen` is forced from 1 to 0, so nothing is stripped
        // and the whole name survives.
        assert_eq!(
            full_fname_path(Path::new("/tmp/srv/mod/nope"), paths("/", "/")),
            "\"/tmp/srv/mod/nope\" (in mymod)"
        );
        // Measured against a real 3.5.0 daemon serving `path = /`:
        // `link_stat "/nope" (in root)`.
        assert_eq!(
            full_fname_path(Path::new("/nope"), paths("/", "/")),
            "\"/nope\" (in mymod)"
        );
        // A deeper module root still renders the absolute name, so the rule is
        // not "always keep the slash" - it is `p1 = curr_dir + module_dirlen`.
        assert_eq!(
            full_fname_path(Path::new("/tmp/srv/mod/nope"), paths("/", "/tmp/srv/mod")),
            "\"/tmp/srv/mod/nope\" (in mymod)"
        );
    }

    /// The same module root one directory down: `p1` is non-empty here, so the
    /// pre-existing `/{p1}/{tail}` arm already reproduced upstream. Negative
    /// control for [`module_rooted_at_slash_keeps_the_leading_slash`] - it must
    /// stay green when that pin's branch is mutated out.
    #[test]
    fn module_rooted_at_slash_below_the_root_is_unchanged() {
        assert_eq!(
            full_fname_path(Path::new("/tmp/srv/nope"), paths("/", "/tmp/srv")),
            "\"/tmp/srv/nope\" (in mymod)"
        );
    }

    #[test]
    fn empty_module_name_still_renders_upstream_shape() {
        // upstream: lp_name() can return an empty string only for a malformed
        // config; `module_id >= 0` still selects the suffix branch.
        assert_eq!(
            full_fname(
                "/srv/mod/f",
                FullFnamePaths::daemon("", Path::new("/srv/mod"), Path::new("/srv/mod"))
            ),
            "\"f\" (in )"
        );
    }

    /// The rendered name is the same `/`-separated name rsync puts on the
    /// wire, so it must not pick up the host separator when the process runs
    /// on Windows. Built with `PathBuf::join` so the input carries the
    /// platform's own separator.
    #[test]
    fn rendered_name_uses_slash_separators_on_every_platform() {
        use std::path::PathBuf;

        let root = PathBuf::from("/srv/mod");
        let curr = root.join("sub");
        let path = curr.join("deep").join("denied.txt");
        assert_eq!(
            full_fname_path(&path, FullFnamePaths::daemon("mymod", &root, &curr)),
            "\"/sub/deep/denied.txt\" (in mymod)"
        );
    }

    #[test]
    fn path_variant_matches_string_variant() {
        assert_eq!(
            full_fname_path(Path::new("/srv/mod/a/b"), paths("/srv/mod", "/srv/mod")),
            full_fname("/srv/mod/a/b", paths("/srv/mod", "/srv/mod"))
        );
    }

    /// The two axes are independent upstream: the prefix comes from `curr_dir`
    /// and `module_dirlen`, the suffix from `module_id`. Outside a daemon
    /// module `module_dirlen` is 0, so the whole working directory is
    /// prefixed onto a relative name while no suffix is appended.
    ///
    /// Ground truth, rsync 3.5.0, `cd /tmp/work && rsync -r nope dst/`:
    ///   rsync: [sender] link_stat "/tmp/work/nope" failed: ...
    #[test]
    fn non_daemon_relative_name_is_anchored_at_curr_dir() {
        assert_eq!(
            full_fname_path(Path::new("nope"), no_module("/tmp/work")),
            "\"/tmp/work/nope\""
        );
        assert_eq!(
            full_fname_path(Path::new("sub/nope"), no_module("/tmp/work")),
            "\"/tmp/work/sub/nope\""
        );
    }

    /// A `./` in the operand must not survive into the rendered name.
    ///
    /// Upstream cleans every flist name with `clean_fname(thisname, 0)`
    /// (`flist.c:1424`); with `CFN_KEEP_DOT_DIRS` unset that discards interior
    /// `"."` dirs (`util1.c:1068-1071`). Ground truth captured from rsync
    /// 3.5.0 pushing `./sub/` from `/tmp/t1141P/work` over a local remote
    /// shell: `send_files failed to open "/tmp/t1141P/work/sub/denied.txt"` -
    /// no `/./` anywhere in the name.
    #[test]
    fn a_dot_component_is_collapsed_out_of_the_rendered_name() {
        assert_eq!(
            full_fname_path(Path::new("./sub/nope"), no_module("/tmp/work")),
            "\"/tmp/work/sub/nope\""
        );
        assert_eq!(
            full_fname_path(Path::new("sub/./nope"), no_module("/tmp/work")),
            "\"/tmp/work/sub/nope\""
        );
        // A name that is nothing but "." is upstream's DOTDIR name and must
        // survive as "." rather than collapsing to the bare directory.
        assert_eq!(
            full_fname_path(Path::new("."), no_module("/tmp/work")),
            "\"/tmp/work/.\""
        );
    }

    #[test]
    fn non_daemon_absolute_path_is_left_absolute() {
        // A client or SSH server process has `module_id < 0`: no suffix, and
        // an absolute name already under `curr_dir` renders to the same
        // absolute string, so a local or SSH run stays byte-identical.
        assert_eq!(
            full_fname_path(
                Path::new("/tmp/work/src/denied.txt"),
                no_module("/tmp/work")
            ),
            "\"/tmp/work/src/denied.txt\""
        );
        // upstream's `*fn == '/'` branch: an absolute name outside `curr_dir`
        // is printed verbatim with no prefix.
        assert_eq!(
            full_fname_path(Path::new("/elsewhere/denied.txt"), no_module("/tmp/work")),
            "\"/elsewhere/denied.txt\""
        );
    }

    /// The non-vacuity companion to the `path = /` case: an ordinary module
    /// root still strips, so the arm above is not simply disabling the strip
    /// for everyone.
    #[test]
    fn module_rooted_below_slash_still_strips() {
        assert_eq!(
            full_fname_path(
                Path::new("/tmp/srv/mod/nope"),
                paths("/tmp/srv/mod", "/tmp/srv/mod")
            ),
            "\"nope\" (in mymod)"
        );
    }

    /// The suffix axis alone: the same prefix rule with and without a module
    /// name produces the same quoted path, differing only in the suffix.
    #[test]
    fn the_module_suffix_is_independent_of_the_prefix() {
        let with_module = full_fname_path(Path::new("/tmp/work/f"), paths("/", "/tmp/work"));
        let without = full_fname_path(Path::new("/tmp/work/f"), no_module("/tmp/work"));
        assert_eq!(with_module, "\"/tmp/work/f\" (in mymod)");
        assert_eq!(without, "\"/tmp/work/f\"");
    }
}
