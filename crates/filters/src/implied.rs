//! Implied-include validation for received file-list names (CVE-2022-29154).
//!
//! When an rsync client pulls from a remote sender, it records each requested
//! source argument as an "implied include". The receiver then validates every
//! incoming file-list name against that set and refuses any name it never
//! asked for. This stops a malicious or buggy sender from injecting extra
//! files that would be written outside the intended destination.
//!
//! The rule set built here mirrors upstream `add_implied_include()`; the
//! per-name test in [`ImpliedIncludes::covers`] mirrors the receiver-side
//! `check_filter(&implied_filter_list, ...)` call.
//!
//! # Upstream Reference
//!
//! - `exclude.c:379` `add_implied_include()` - turns each requested source arg
//!   into one or more `FILTRULE_INCLUDE` rules (basename/relative handling,
//!   parent-dir implication for `--relative`, a trailing `/**` (`--recursive`)
//!   or `/*` (`--dirs`) rule, wildcard args producing `FILTRULE_WILD` rules).
//! - `flist.c:1026` `recv_file_entry()` - rejects any received name for which
//!   `check_filter(&implied_filter_list, ...) <= 0` (no include match).
//! - `options.c:2510-2513` - `trust_sender_args` disables the mechanism for
//!   local/`--trust-sender`/server/old-style/`files-from-host` transfers; the
//!   caller reflects that by not building the list at all in those cases.

use std::collections::HashSet;
use std::path::Path;

use crate::compiled::CompiledRule;
use crate::{FilterError, FilterRule};

/// Transfer options that shape how source args become implied-include rules.
///
/// These correspond to the upstream globals consulted by `add_implied_include`:
/// `relative_paths`, `recurse`, `xfer_dirs`, and the daemon-module flag.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ImpliedIncludeOptions {
    /// `--relative` (`relative_paths`): keep the path after a `/./` pivot and
    /// imply every parent directory instead of reducing the arg to its
    /// basename.
    pub relative: bool,
    /// `--recursive` (`recurse`): also add a trailing `arg/**` rule so the
    /// requested directory's whole subtree is accepted.
    pub recurse: bool,
    /// `--dirs` (`xfer_dirs`): add a trailing `arg/*` rule accepting the
    /// requested directory's immediate contents.
    pub dirs: bool,
    /// The source arg is a daemon `module/path` spec: strip the leading module
    /// name (everything up to and including the first `/`).
    ///
    /// upstream: `exclude.c:396-401` `skip_daemon_module`.
    pub skip_daemon_module: bool,
}

/// Set of implied-include rules built from a client's requested source args.
///
/// Construct with [`ImpliedIncludes::from_args`], then test each received
/// file-list name with [`ImpliedIncludes::covers`]. An empty set means the
/// mechanism is inactive and no name should be rejected.
#[derive(Debug, Default)]
pub struct ImpliedIncludes {
    opts: ImpliedIncludeOptions,
    rules: Vec<CompiledRule>,
    seen: HashSet<String>,
}

impl ImpliedIncludes {
    /// Creates an empty set for the given transfer options.
    #[must_use]
    pub fn new(opts: ImpliedIncludeOptions) -> Self {
        Self {
            opts,
            rules: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Builds the implied-include set from `args` under `opts`.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] only if an implied pattern cannot be compiled
    /// even after literal escaping (not expected in practice).
    pub fn from_args<I, S>(opts: ImpliedIncludeOptions, args: I) -> Result<Self, FilterError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut this = Self::new(opts);
        for arg in args {
            this.add_arg(arg.as_ref())?;
        }
        Ok(this)
    }

    /// Adds the implied-include rules for a single requested source `arg`.
    ///
    /// upstream: `exclude.c:379` `add_implied_include()`.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] if a synthesized pattern cannot be compiled even
    /// after literal escaping.
    pub fn add_arg(&mut self, arg: &str) -> Result<(), FilterError> {
        let mut arg = arg;

        // upstream: exclude.c:396-401 - strip the daemon module name.
        if self.opts.skip_daemon_module {
            arg = arg.split_once('/').map_or("", |(_, rest)| rest);
        }

        // upstream: exclude.c:403-408 - --relative keeps the path after a
        // "/./" pivot; otherwise the arg is reduced to its basename.
        if self.opts.relative {
            if let Some(idx) = arg.find("/./") {
                arg = &arg[idx + 3..];
            }
        } else if let Some(idx) = arg.rfind('/') {
            arg = &arg[idx + 1..];
        }

        // upstream: exclude.c:410-411 - a bare "." arg contributes no name rule.
        if arg == "." {
            arg = "";
        }

        // upstream: exclude.c:412-491 - normalise the arg into an anchored
        // pattern, collapsing "//", "/./" and trailing "/" the way the C loop
        // does. Empty and "." segments are dropped.
        let segments: Vec<&str> = arg
            .split('/')
            .filter(|seg| !seg.is_empty() && *seg != ".")
            .collect();

        if !segments.is_empty() {
            let base = format!("/{}", segments.join("/"));
            self.push_rule(&base, false)?;

            // upstream: exclude.c:497-527 - with --relative every parent
            // directory of the arg is implied as a directory-only include.
            if self.opts.relative {
                for depth in 1..segments.len() {
                    let parent = format!("/{}/", segments[..depth].join("/"));
                    self.push_rule(&parent, true)?;
                }
            }
        }

        // upstream: exclude.c:531-567 - --recursive adds "arg/**" and --dirs
        // adds "arg/*" (an empty arg yields "/**" or "/*", accepting the tree).
        if self.opts.recurse || self.opts.dirs {
            let base = if segments.is_empty() {
                String::new()
            } else {
                format!("/{}", segments.join("/"))
            };
            let suffix = if self.opts.recurse { "**" } else { "*" };
            let pattern = format!("{base}/{suffix}");
            self.push_rule(&pattern, false)?;
        }

        Ok(())
    }

    /// Returns `true` when no rules were built (mechanism inactive).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Tests whether `path` is covered by any implied-include rule.
    ///
    /// Equivalent to upstream `check_filter(&implied_filter_list, ...) > 0`:
    /// the list holds only include rules, so a match means the name was
    /// requested and a non-match means it should be rejected. Matching uses
    /// `check_descendants = false`, giving exactly upstream `rule_matches()`
    /// semantics (no synthetic `pattern/**` descendant matchers).
    #[must_use]
    pub fn covers(&self, path: &Path, is_dir: bool) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.matches(path, is_dir, false))
    }

    /// Compiles and stores one anchored include rule, de-duplicating repeats.
    ///
    /// When the pattern contains a live (unescaped) `[`, an additional rule with
    /// every such `[` escaped is stored so the *literal* bracketed name is also
    /// accepted. A remote shell may return a file matching the literal brackets
    /// when a `[foo]` glob idiom failed to expand, so upstream adds both the
    /// wildcard rule and the escaped-literal rule.
    ///
    /// upstream: `exclude.c:312` `maybe_add_literal_brackets_rule()`, invoked
    /// after each implied rule with a live `[` (exclude.c:494, 526, 569).
    fn push_rule(&mut self, pattern: &str, directory_only: bool) -> Result<(), FilterError> {
        if self.seen.insert(pattern.to_owned()) {
            let compiled = CompiledRule::new(FilterRule::include(pattern))?;
            debug_assert_eq!(compiled.is_directory_only(), directory_only);
            self.rules.push(compiled);
        }
        if let Some(escaped) = escape_live_brackets(pattern) {
            if self.seen.insert(escaped.clone()) {
                let compiled = CompiledRule::new(FilterRule::include(escaped))?;
                debug_assert_eq!(compiled.is_directory_only(), directory_only);
                self.rules.push(compiled);
            }
        }
        Ok(())
    }
}

/// Escapes every live (unescaped) `[` in `pattern` as `\[`, returning `None`
/// when the pattern has no live `[`.
///
/// upstream: `exclude.c:312` `maybe_add_literal_brackets_rule()` - a `\`
/// consumes the following byte (so an already-escaped `\[` is left alone), and
/// each remaining `[` is prefixed with a backslash so it matches literally.
fn escape_live_brackets(pattern: &str) -> Option<String> {
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(pattern.len() + 4);
    let mut cut = 0;
    let mut i = 0;
    let mut changed = false;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            // Skip the escape pair verbatim so `\[` is not re-escaped.
            i += 2;
        } else if bytes[i] == b'[' {
            out.push_str(&pattern[cut..i]);
            out.push('\\');
            cut = i;
            changed = true;
            i += 1;
        } else {
            i += 1;
        }
    }
    if !changed {
        return None;
    }
    out.push_str(&pattern[cut..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{ImpliedIncludeOptions, ImpliedIncludes, escape_live_brackets};
    use std::path::Path;

    fn covers(implied: &ImpliedIncludes, name: &str, is_dir: bool) -> bool {
        implied.covers(Path::new(name), is_dir)
    }

    #[test]
    fn recursive_pull_accepts_request_and_subtree_but_rejects_injection() {
        // `oc-rsync -r host:dir dest` requests only `dir`; the sender must not
        // be able to smuggle an unrelated `evil` (CVE-2022-29154).
        let opts = ImpliedIncludeOptions {
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["dir"]).unwrap();
        assert!(!implied.is_empty());
        assert!(covers(&implied, "dir", true));
        assert!(covers(&implied, "dir/file", false));
        assert!(covers(&implied, "dir/sub/deep", false));
        assert!(!covers(&implied, "evil", false));
        assert!(!covers(&implied, "dirEVIL", false));
    }

    #[test]
    fn non_recursive_pull_rejects_children_like_upstream() {
        // Without -r/-d upstream adds only `/dir`; a child arriving anyway is
        // an injection and must be rejected.
        let implied =
            ImpliedIncludes::from_args(ImpliedIncludeOptions::default(), ["dir"]).unwrap();
        assert!(covers(&implied, "dir", true));
        assert!(!covers(&implied, "dir/file", false));
    }

    #[test]
    fn non_relative_pull_reduces_arg_to_basename() {
        // `host:a/b` (no --relative) is sent as top-level `b`, so `b` is
        // implied and the parent `a` is not a valid received name.
        let opts = ImpliedIncludeOptions {
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["a/b"]).unwrap();
        assert!(covers(&implied, "b", true));
        assert!(covers(&implied, "b/c", false));
        assert!(!covers(&implied, "a", true));
        assert!(!covers(&implied, "a/evil", false));
    }

    #[test]
    fn relative_pull_implies_every_parent_directory() {
        // `-R host:a/b/c` keeps the full path and implies parents `a` and
        // `a/b` as directories.
        let opts = ImpliedIncludeOptions {
            relative: true,
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["a/b/c"]).unwrap();
        assert!(covers(&implied, "a", true));
        assert!(covers(&implied, "a/b", true));
        assert!(covers(&implied, "a/b/c", true));
        assert!(covers(&implied, "a/b/c/leaf", false));
        // A parent implied as a directory must not admit an unrequested file
        // sitting beside the requested path.
        assert!(!covers(&implied, "a/evil", false));
        assert!(!covers(&implied, "a/b/evil", false));
    }

    #[test]
    fn relative_pull_honours_dot_pivot() {
        // `-R host:src/./dir` pivots at "/./"; only `dir` and its subtree are
        // implied, not `src`.
        let opts = ImpliedIncludeOptions {
            relative: true,
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["src/./dir"]).unwrap();
        assert!(covers(&implied, "dir", true));
        assert!(covers(&implied, "dir/file", false));
        assert!(!covers(&implied, "src", true));
    }

    #[test]
    fn wildcard_arg_stays_active_and_wildcard_aware() {
        // upstream 3.4.4 does NOT disable the check for wildcard args: it emits
        // a FILTRULE_WILD rule (exclude.c:415). A `d*` request admits matching
        // names and still rejects non-matching injections.
        let opts = ImpliedIncludeOptions {
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["d*"]).unwrap();
        assert!(covers(&implied, "data", true));
        assert!(covers(&implied, "data/file", false));
        assert!(!covers(&implied, "evil", false));
    }

    #[test]
    fn bare_star_arg_admits_everything() {
        // A top-level `*` naturally matches every name via `/ *` + `/ */**`,
        // reproducing upstream's effectively-permissive behaviour for `*`.
        let opts = ImpliedIncludeOptions {
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["*"]).unwrap();
        assert!(covers(&implied, "anything", false));
        assert!(covers(&implied, "any/thing", false));
    }

    #[test]
    fn trailing_slash_source_admits_bare_child_names() {
        // A trailing-slash source (`host:dir/`) transfers the directory's
        // CONTENTS, so the received names are bare children. upstream reduces
        // the arg to "" and (with -r) builds `/**`, admitting them. Regression
        // for over-rejecting `file`/`one` on a trailing-slash pull.
        let opts = ImpliedIncludeOptions {
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["/abs/A weird)name/"]).unwrap();
        assert!(covers(&implied, "file", false));
        assert!(covers(&implied, "one", false));
        assert!(covers(&implied, "sub/deep", false));
    }

    #[test]
    fn files_from_relative_entries_admit_requested_tree() {
        // --files-from implies --relative and disables recursion in favour of
        // --dirs (options.c:2169-2173,2205-2206). `from/./` reduces to `/*`,
        // admitting every top-level name the whole-dir entry requested.
        let opts = ImpliedIncludeOptions {
            relative: true,
            recurse: false,
            dirs: true,
            skip_daemon_module: false,
        };
        let implied = ImpliedIncludes::from_args(
            opts,
            [
                "from/./",
                "from/./dir/subdir",
                "from/./dir/subdir/subsubdir2/",
                "from/./dir/subdir/foobar.baz",
            ],
        )
        .unwrap();
        // `from/./` -> `/*` admits every top-level requested name.
        assert!(covers(&implied, "empty", false));
        // Implied parent dirs and the listed paths.
        assert!(covers(&implied, "dir", true));
        assert!(covers(&implied, "dir/subdir", true));
        assert!(covers(&implied, "dir/subdir/foobar.baz", false));
        assert!(covers(&implied, "dir/subdir/subsubdir2", true));
        assert!(covers(&implied, "dir/subdir/subsubdir2/y", false));
    }

    #[test]
    fn files_from_without_whole_dir_entry_rejects_out_of_tree_name() {
        // Without a `from/./` whole-tree entry, a name outside every requested
        // path is still rejected - the check keeps its teeth under --files-from.
        let opts = ImpliedIncludeOptions {
            relative: true,
            recurse: false,
            dirs: true,
            skip_daemon_module: false,
        };
        let implied = ImpliedIncludes::from_args(opts, ["from/./dir/subdir"]).unwrap();
        assert!(covers(&implied, "dir", true));
        assert!(covers(&implied, "dir/subdir", true));
        assert!(covers(&implied, "dir/subdir/child", false));
        assert!(!covers(&implied, "empty", false));
        assert!(!covers(&implied, "other/evil", false));
    }

    #[test]
    fn dot_arg_with_recurse_accepts_whole_tree() {
        let opts = ImpliedIncludeOptions {
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["."]).unwrap();
        assert!(covers(&implied, "anything", false));
        assert!(covers(&implied, "deep/path/file", false));
    }

    #[test]
    fn daemon_module_prefix_is_stripped() {
        // A daemon `module/dir` arg drops the module name before building rules.
        let opts = ImpliedIncludeOptions {
            recurse: true,
            skip_daemon_module: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["mod/dir"]).unwrap();
        assert!(covers(&implied, "dir", true));
        assert!(covers(&implied, "dir/file", false));
        assert!(!covers(&implied, "mod", true));
    }

    #[test]
    fn empty_args_leave_set_inactive() {
        let implied =
            ImpliedIncludes::from_args(ImpliedIncludeOptions::default(), Vec::<String>::new())
                .unwrap();
        assert!(implied.is_empty());
    }

    #[test]
    fn live_bracket_arg_also_admits_literal_name() {
        // upstream: exclude.c:312 maybe_add_literal_brackets_rule() - an arg
        // with a live `[` gets an extra escaped-bracket rule so the literal
        // name is admitted when the glob idiom failed to expand remotely. It is
        // stricter than a wildcard, never more permissive.
        let opts = ImpliedIncludeOptions {
            recurse: true,
            ..Default::default()
        };
        let implied = ImpliedIncludes::from_args(opts, ["a[b"]).unwrap();
        assert!(covers(&implied, "a[b", true));
        assert!(!covers(&implied, "evil", false));
    }

    #[test]
    fn escape_live_brackets_escapes_only_live_brackets() {
        assert_eq!(escape_live_brackets("a[b").as_deref(), Some("a\\[b"));
        assert_eq!(
            escape_live_brackets("/x[y]z/**").as_deref(),
            Some("/x\\[y]z/**")
        );
        // Already-escaped `\[` is left alone; no live `[` means no rewrite.
        assert_eq!(escape_live_brackets("a\\[b"), None);
        assert_eq!(escape_live_brackets("plain/name"), None);
    }
}
