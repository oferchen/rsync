//! Per-directory scoped filter chain with push/pop semantics.
//!
//! [`FilterChain`] extends [`FilterSet`] with per-directory merge file support,
//! implementing upstream rsync's `push_local_filters()` / `pop_local_filters()`
//! pattern from `exclude.c`. When rsync enters a directory, it reads any
//! per-directory merge files (e.g., `.rsync-filter`) and pushes their rules
//! onto a scoped stack. When leaving, the rules are popped. Per-directory rules
//! take priority over global rules (first-match-wins within each layer,
//! innermost directory first).
//!
//! # Chain of Responsibility
//!
//! Evaluation proceeds from the innermost (most recently pushed) directory
//! scope outward to the global base rules. Within each scope, rules use
//! first-match-wins semantics. The first matching rule across all scopes
//! determines the outcome. If no rule matches anywhere, the default is to
//! include the path.
//!
//! # Submodules
//!
//! - `config` - [`DirMergeConfig`] for per-directory merge file behaviour
//! - `scope` - [`DirFilterGuard`] and internal per-directory scope handling
//! - `error` - [`FilterChainError`] for I/O and parse failures
//!
//! # Upstream References
//!
//! - `exclude.c:push_local_filters()` - enter directory, read merge files
//! - `exclude.c:pop_local_filters()` - leave directory, restore state
//! - `exclude.c:change_local_filter_dir()` - depth-tracked push/pop

mod config;
mod error;
mod scope;

#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::merge::parse::{parse_rules, parse_rules_no_prefixes, parse_rules_word_split};
use crate::{FilterAction, FilterRule, FilterSet};

pub use config::DirMergeConfig;
pub use error::FilterChainError;
pub use scope::DirFilterGuard;

use scope::{ClearedScope, DirScope, scope_has_deletion_match, scope_has_transfer_match};

/// Per-directory scoped filter chain with push/pop semantics.
///
/// `FilterChain` manages a stack of per-directory filter scopes on top of a
/// global base [`FilterSet`]. When evaluating a path, scopes are checked from
/// innermost (most recently pushed) to outermost, then the global rules.
/// The first matching rule across all layers wins.
///
/// This mirrors upstream rsync's `push_local_filters()` / `pop_local_filters()`
/// from `exclude.c`, which maintains a stack of per-directory merge file rules
/// that are pushed when entering directories and popped when leaving.
///
/// # Examples
///
/// ```
/// use filters::{FilterChain, FilterRule, FilterSet, DirMergeConfig};
/// use std::path::Path;
///
/// let global = FilterSet::from_rules([
///     FilterRule::exclude("*.bak"),
/// ]).unwrap();
///
/// let mut chain = FilterChain::new(global);
/// chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));
///
/// // Global rules apply everywhere
/// assert!(!chain.allows(Path::new("file.bak"), false));
/// assert!(chain.allows(Path::new("file.txt"), false));
/// ```
#[derive(Clone, Debug)]
pub struct FilterChain {
    global: FilterSet,
    merge_configs: Vec<DirMergeConfig>,
    /// Ordered from outermost to innermost; evaluation iterates in reverse.
    scopes: Vec<DirScope>,
    /// Ancestor scopes removed by a `!` (clear-list) rule, tagged with the
    /// depth at which the clear fired so they can be restored when that
    /// directory is left. Mirrors upstream `exclude.c:pop_local_filters()`,
    /// which rebuilds the pre-descent mergelist for sibling directories.
    cleared_scopes: Vec<ClearedScope>,
    current_depth: usize,
    /// Mirrors upstream's `delete_excluded` global. When `true`, per-token
    /// rules expanded from merge files acquire an implicit FILTRULE_SENDER_SIDE
    /// flag so that the receiver's delete-pass does not skip matching files.
    ///
    /// upstream: exclude.c:1324-1332 parse_rule_tok - the OR fires for every
    /// parsed rule under --delete-excluded except the merge/dir-merge wrappers
    /// themselves.
    delete_excluded: bool,
    /// Transfer-root directory used to re-anchor leading-`/` rules read from a
    /// per-directory merge file to the merge file's own directory.
    ///
    /// upstream: exclude.c:200-228 add_rule - under `XFLG_ANCHORED2ABS` a
    /// leading-`/` rule in a per-dir merge file is rewritten so its pattern is
    /// prefixed with the merge file's directory (relative to the module root).
    /// `/file1` read from `foo/.filt` therefore matches `foo/file1`, not a
    /// top-level `file1`. When `None`, no re-anchoring is performed (preserving
    /// the behaviour of callers that pass merge directories already relative to
    /// the transfer root, e.g. unit tests).
    transfer_root: Option<PathBuf>,
}

impl FilterChain {
    /// Creates a new filter chain with the given global rules.
    ///
    /// The global rules serve as the base layer. Per-directory scopes are
    /// pushed on top during traversal.
    #[must_use]
    pub fn new(global: FilterSet) -> Self {
        Self {
            global,
            merge_configs: Vec::new(),
            scopes: Vec::new(),
            cleared_scopes: Vec::new(),
            current_depth: 0,
            delete_excluded: false,
            transfer_root: None,
        }
    }

    /// Creates an empty filter chain with no global rules.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(FilterSet::default())
    }

    /// Adds a per-directory merge file configuration.
    ///
    /// When [`enter_directory`](Self::enter_directory) is called, the chain
    /// will look for files matching this configuration's filename and parse
    /// their rules into scoped filter sets.
    pub fn add_merge_config(&mut self, config: DirMergeConfig) {
        self.merge_configs.push(config);
    }

    /// Reports whether any per-directory merge file configurations are
    /// registered on this chain.
    ///
    /// The receiver's delete pass consults this to decide whether it must
    /// reload per-directory merge rules while scanning each destination
    /// directory (mirroring upstream `delete.c`/`exclude.c`
    /// `change_local_filter_dir` -> `push_local_filters`). When no merge
    /// configs exist the shared global rule snapshot is sufficient and the
    /// per-directory reload is skipped.
    #[must_use]
    pub fn has_per_dir_merge(&self) -> bool {
        !self.merge_configs.is_empty()
    }

    /// Marks this chain as operating under `--delete-excluded`.
    ///
    /// When enabled, rules expanded out of per-directory merge files acquire
    /// an implicit sender-side flag, mirroring upstream's per-token OR in
    /// `exclude.c:1324-1332 parse_rule_tok`. Without this, the receiver's
    /// delete-pass would treat merge-expanded excludes as receiver-side
    /// excludes and skip the matching files instead of deleting them.
    #[must_use]
    pub const fn with_delete_excluded(mut self, delete_excluded: bool) -> Self {
        self.delete_excluded = delete_excluded;
        self
    }

    /// Reports whether implicit per-token sender-side promotion is active.
    #[must_use]
    pub const fn delete_excluded(&self) -> bool {
        self.delete_excluded
    }

    /// Records the transfer-root directory so that leading-`/` rules read from
    /// per-directory merge files are re-anchored to the merge file's directory.
    ///
    /// Callers walking a tree (e.g. the sender generator) pass the same base
    /// directory they strip from each path before calling
    /// [`allows`](Self::allows). With the root set, a `- /file1` rule inside
    /// `<root>/foo/.filt` is rewritten to match `foo/file1`, mirroring upstream
    /// rsync's `XFLG_ANCHORED2ABS` handling in `exclude.c:add_rule`.
    pub fn set_transfer_root(&mut self, root: impl Into<PathBuf>) {
        self.transfer_root = Some(root.into());
    }

    /// Returns the directory of a per-dir merge file relative to the transfer
    /// root, as a forward-slash string, or `None` when re-anchoring is not in
    /// effect (no root set, the path is the root itself, or it is not below the
    /// root). The returned prefix never has leading or trailing slashes.
    fn merge_rel_dir(&self, directory: &Path) -> Option<String> {
        let root = self.transfer_root.as_ref()?;
        let rel = directory.strip_prefix(root).ok()?;
        let joined = path_to_forward_slash(rel);
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }

    /// Returns `true` if the path should be included in the transfer.
    ///
    /// Evaluates per-directory scopes from innermost to outermost, then
    /// global rules. First matching rule wins. Paths that match no rule
    /// are included by default.
    ///
    /// This is the traversal-driven sender entry point: synthetic
    /// `pattern/**` descendant matchers (produced for anchored excludes
    /// like `- /bar`) are skipped because the walk itself handles
    /// descendant exclusion by not descending into excluded directories.
    /// Mirrors upstream `exclude.c:rule_matches()` which has no descendant
    /// matching at all.
    ///
    /// Scopes pushed by non-inheriting merge configs (`FILTRULE_NO_INHERIT`,
    /// e.g. `:C`) are consulted only at the depth they were pushed at, so
    /// rules from a parent directory's `.cvsignore` do not leak into a
    /// deeper child walk.
    ///
    /// `is_dir` should be `true` when the path refers to a directory.
    #[must_use]
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool {
        for scope in self.scopes.iter().rev() {
            if !self.scope_applies_here(scope) {
                continue;
            }
            if !scope.filter_set.is_empty()
                && scope_has_transfer_match(&scope.filter_set, path, is_dir)
            {
                return scope.filter_set.allows_during_traversal(path, is_dir);
            }
        }

        self.global.allows_during_traversal(path, is_dir)
    }

    /// Returns `true` if deleting the path on the receiver is permitted.
    ///
    /// Evaluates per-directory scopes from innermost to outermost, then
    /// global rules. A path may be deleted when it is included by
    /// receiver-side rules and no protect rule matches.
    ///
    /// Non-inheriting scopes follow the same depth-restricted lookup as
    /// [`allows`](Self::allows).
    #[must_use]
    pub fn allows_deletion(&self, path: &Path, is_dir: bool) -> bool {
        // upstream: exclude.c:rule_matches() with the `delete_excluded` global -
        // an entry that an exclude rule would normally protect from deletion
        // becomes deletable under `--delete-excluded` (a `protect`/`P` rule
        // still protects it). Mirror that by OR-ing in the
        // "excluded-removed" decision whenever `delete_excluded` is set, on
        // whichever scope (or the global set) decides the path.
        let result = (|| {
            for scope in self.scopes.iter().rev() {
                if !self.scope_applies_here(scope) {
                    continue;
                }
                if !scope.filter_set.is_empty()
                    && scope_has_deletion_match(&scope.filter_set, path, is_dir)
                {
                    let base = scope
                        .filter_set
                        .allows_deletion_during_traversal(path, is_dir);
                    return base
                        || (self.delete_excluded
                            && scope
                                .filter_set
                                .allows_deletion_when_excluded_removed(path, is_dir));
                }
            }

            let base = self.global.allows_deletion(path, is_dir);
            base || (self.delete_excluded
                && self
                    .global
                    .allows_deletion_when_excluded_removed(path, is_dir))
        })();

        logging::debug_log!(
            Filter,
            3,
            "allows_deletion({:?}, is_dir={is_dir}) delete_excluded={} -> {result}",
            path,
            self.delete_excluded
        );
        result
    }

    /// Returns `true` if the given scope is in effect at the chain's
    /// current depth.
    ///
    /// Inheriting scopes always apply. Non-inheriting scopes only apply
    /// at the depth they were pushed at; deeper directories must look
    /// past them. Mirrors upstream `exclude.c:push_local_filters()` which
    /// substitutes the inherited list with `lp->head = NULL` for
    /// `FILTRULE_NO_INHERIT` rules before descending.
    fn scope_applies_here(&self, scope: &DirScope) -> bool {
        scope.inherits || scope.depth == self.current_depth
    }

    /// Enters a directory, reading any per-directory merge files and pushing
    /// their rules onto the scope stack.
    ///
    /// For each configured merge file, checks whether the file exists in the
    /// given directory. If found, parses its rules and pushes a new scope.
    /// Returns a [`DirFilterGuard`] that pops the scopes when dropped.
    ///
    /// # Arguments
    ///
    /// * `directory` - Absolute path to the directory being entered
    ///
    /// # Errors
    ///
    /// Returns [`FilterChainError`] if a merge file exists but cannot be read
    /// or contains invalid syntax.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `exclude.c:push_local_filters()` which reads each registered
    /// per-dir merge file from the current directory and pushes its rules.
    pub fn enter_directory(
        &mut self,
        directory: &Path,
    ) -> Result<DirFilterGuard, FilterChainError> {
        self.current_depth += 1;
        let depth = self.current_depth;
        let mut pushed_count = 0;

        // upstream: exclude.c:200-228 - leading-`/` rules in a per-dir merge
        // file are re-anchored to the merge file's directory (relative to the
        // module root). Compute that directory once for every merge file read
        // in this directory.
        let rel_dir_prefix = self.merge_rel_dir(directory);

        for config_index in 0..self.merge_configs.len() {
            let config = &self.merge_configs[config_index];
            let merge_path = directory.join(config.filename());

            // upstream: exclude.c:push_local_filters() - parse_filter_file()
            // silently skips missing files
            let content = match fs::read_to_string(&merge_path) {
                Ok(content) => content,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => continue,
                Err(e) => {
                    self.pop_scopes_at_depth(depth);
                    self.current_depth -= 1;
                    return Err(FilterChainError::Io {
                        path: merge_path,
                        source: e,
                    });
                }
            };

            // upstream: exclude.c:1252-1254 - dir-merge files registered as
            // `:C` (FILTRULE_CVS_IGNORE) parse their content as whitespace
            // separated tokens with every token implicitly an exclude.
            // Standard parse_rules would reject names like `one-in-one-out`
            // as "Unknown filter rule", aborting the walk.
            let rules = if config.no_prefixes() {
                // upstream: exclude.c:1116-1133 - the `-`/`+` modifier on the
                // dir-merge template skips short-prefix dispatch, so each
                // line becomes a literal exclude (or include for `+`). When
                // CVS_IGNORE is also inherited (e.g. `:-C`), a bare `!` line
                // clears the list per FILTRULE_CLEAR_LIST. The `w` modifier
                // (e.g. `:w-`) tokenises on whitespace instead of per line.
                parse_rules_no_prefixes(
                    &content,
                    &merge_path,
                    config.no_prefixes_include(),
                    config.cvs_mode(),
                    config.word_split(),
                )
            } else if config.cvs_mode() {
                parse_cvs_ignore_tokens(&content)
            } else if config.word_split() {
                // upstream: exclude.c:1499 - the `w` modifier splits the merge
                // file on any whitespace and parses every token as its own
                // rule (comments are not stripped, line 1514).
                match parse_rules_word_split(&content, &merge_path) {
                    Ok(rules) => rules,
                    Err(e) => {
                        self.pop_scopes_at_depth(depth);
                        self.current_depth -= 1;
                        return Err(FilterChainError::Parse {
                            path: merge_path,
                            message: e.to_string(),
                        });
                    }
                }
            } else {
                match parse_rules(&content, &merge_path) {
                    Ok(rules) => rules,
                    Err(e) => {
                        self.pop_scopes_at_depth(depth);
                        self.current_depth -= 1;
                        return Err(FilterChainError::Parse {
                            path: merge_path,
                            message: e.to_string(),
                        });
                    }
                }
            };

            // upstream: exclude.c:1419-1428 - a `:` (FILTRULE_PERDIR_MERGE)
            // directive inside a merge file registers a new per-directory
            // merge that takes effect for the current directory and below.
            // `:C` is the common case: register `.cvsignore` as a CVS-style
            // ignore list (no_inherit, word_split). Standard FilterSet
            // compilation drops DirMerge rules, so handle them here by
            // loading the named file from the current directory and folding
            // its tokens into this scope.
            let (rules, mut dir_merge_descriptors) = split_dir_merge_rules(rules);

            // upstream: exclude.c:1393-1401 parse_filter_str() - a `!`
            // (FILTRULE_CLEAR_LIST) in a per-directory merge file runs
            // `pop_filter_list(listp)` and THEN `listp->head = NULL`, so it
            // drops the mergelist's inherited ancestor rules, not just this
            // directory's own section. Only an inheriting config accumulates
            // ancestor rules to clear (a non-inheriting `:C`-style list is
            // re-read fresh per directory), so gate on `config.inherits()` to
            // match the engine local-copy path (context_impl/transfer.rs, which
            // clears `layers[index]` only when `rule.options().inherit_rules()`).
            let clears_inherited = config.inherits()
                && rules
                    .iter()
                    .any(|r| matches!(r.action(), FilterAction::Clear));

            if rules.is_empty() && dir_merge_descriptors.is_empty() && !config.excludes_self() {
                continue;
            }

            let config = &self.merge_configs[config_index];
            let delete_excluded = self.delete_excluded;

            // upstream: exclude.c:1293-1303 - a `dir-merge` directive parsed from
            // inside a side-restricted per-directory merge (e.g. `:s .filt`)
            // inherits the container's FILTRULES_SIDES, so the rules it later
            // loads (and every descendant reload of the registered config) become
            // side-restricted too. Without this, a nested `dir-merge .filt2`
            // declared inside a `:s` merge stays two-sided and its `- *.deep`
            // wrongly protects flist-absent files from receiver-side deletion.
            if config.is_sender_only() {
                for descriptor in &mut dir_merge_descriptors {
                    descriptor.sender_only = true;
                }
            } else if config.is_receiver_only() {
                for descriptor in &mut dir_merge_descriptors {
                    descriptor.receiver_only = true;
                }
            }

            // upstream: exclude.c:200-228 add_rule - FILTRULE_ABS_PATH keeps a
            // leading `/` anchored to the transfer root, so a dir-merge declared
            // with the `/` modifier skips the merge-directory re-anchoring.
            let reanchor_dir = if config.is_anchor_root() {
                None
            } else {
                rel_dir_prefix.as_deref()
            };
            let mut modified_rules: Vec<FilterRule> = rules
                .into_iter()
                .map(|rule| config.apply_modifiers(rule))
                .map(|rule| reanchor_merge_rule(rule, reanchor_dir))
                .map(|rule| apply_merge_implicit_sender_side(rule, delete_excluded))
                .collect();

            // upstream: exclude.c - FILTRULE_EXCLUDE_SELF handling
            if config.excludes_self() {
                modified_rules.push(FilterRule::exclude(config.filename().to_owned()));
            }

            // Capture before the mutable-borrow clear below drops the `config`
            // reference into `self.merge_configs`.
            let config_inherits = config.inherits();

            let filter_set = match FilterSet::from_rules(modified_rules) {
                Ok(set) => set,
                Err(e) => {
                    self.pop_scopes_at_depth(depth);
                    self.current_depth -= 1;
                    return Err(FilterChainError::Filter(e));
                }
            };

            // upstream: exclude.c:1393-1401 - drop the inherited ancestor rules
            // of THIS mergelist (same `config_index`) before the current
            // directory's own section is pushed. The removed scopes are stashed
            // in `cleared_scopes` and restored when this directory is left, so a
            // sibling directory without a `!` still sees the parent rules
            // (exclude.c:pop_local_filters rebuilds the pre-descent mergelist).
            // Runs even when `filter_set` is empty (a lone `!`), so the clear is
            // never a no-op. FilterSet::from_rules already applied the
            // within-file clear (rules before `!` in this same file).
            if clears_inherited {
                self.clear_inherited_scopes(config_index, depth);
            }

            if !filter_set.is_empty() {
                self.scopes.push(DirScope {
                    depth,
                    filter_set,
                    inherits: config_inherits,
                    config_index: Some(config_index),
                });
                pushed_count += 1;
            }

            // upstream: exclude.c:1419-1428 - for each `:`/`:C` directive
            // encountered in the merge file body, attempt to load the named
            // file from the current directory now. The dir-merge rule itself
            // carries the modifier flags (cvs_mode, no_inherit) that decide
            // how to parse the file and whether descendant scopes inherit.
            for descriptor in dir_merge_descriptors {
                // upstream: exclude.c:294 - a `dir-merge` directive parsed from
                // inside a merge file is appended to the global
                // `mergelist_parents`, so `push_local_filters()` re-reads it in
                // every descendant directory. An inheriting dir-merge must
                // therefore become a persistent per-directory config, not just
                // a one-shot read of the current directory. Non-inheriting
                // (`:C`, no-inherit) variants stay one-shot to avoid leaking
                // into sibling subtrees. Dedupe by filename like upstream's
                // "already mentioned" guard (exclude.c:262-279).
                if !descriptor.no_inherit
                    && !self
                        .merge_configs
                        .iter()
                        .any(|c| c.filename() == descriptor.filename)
                {
                    self.merge_configs.push(
                        DirMergeConfig::new(descriptor.filename.clone())
                            .with_cvs_mode(descriptor.cvs_mode)
                            .with_inherit(true)
                            .with_sender_only(descriptor.sender_only)
                            .with_receiver_only(descriptor.receiver_only)
                            // upstream: exclude.c - FILTRULE_NO_PREFIXES / ABS_PATH
                            // from the `:`-rule template propagate to the
                            // registered per-directory config.
                            .with_no_prefixes(
                                descriptor.no_prefixes,
                                descriptor.no_prefixes_include,
                            )
                            .with_word_split(descriptor.word_split)
                            .with_anchor_root(descriptor.abs_path),
                    );
                }
                pushed_count += self.load_inline_dir_merge(directory, depth, &descriptor)?;
            }
        }

        Ok(DirFilterGuard {
            depth,
            pushed_count,
        })
    }

    /// Loads a `:C` style per-directory merge declared inside another merge
    /// file. Returns the number of scopes successfully pushed.
    ///
    /// Reads `directory/<filename>` if present and folds its rules into a
    /// new scope at `depth`. CVS-mode entries are tokenised with
    /// [`parse_cvs_ignore_tokens`]; other dir-merge variants reuse the
    /// standard rule parser. The scope inherits to descendant directories
    /// only when the source rule does not carry the no-inherit modifier.
    ///
    /// upstream: exclude.c:1419-1428 - a `:` directive inside a merge file
    /// expands to a fresh per-directory merge for the current scope.
    fn load_inline_dir_merge(
        &mut self,
        directory: &Path,
        depth: usize,
        descriptor: &InlineDirMerge,
    ) -> Result<usize, FilterChainError> {
        let merge_path = directory.join(&descriptor.filename);
        let content = match fs::read_to_string(&merge_path) {
            Ok(content) => content,
            // upstream: exclude.c:push_local_filters() - parse_filter_file()
            // silently skips missing files. Mirror that here so a `:C`
            // without an accompanying `.cvsignore` is a no-op.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Ok(0),
            Err(e) => {
                self.pop_scopes_at_depth(depth);
                self.current_depth -= 1;
                return Err(FilterChainError::Io {
                    path: merge_path,
                    source: e,
                });
            }
        };

        // upstream: exclude.c:1116-1133 - a dir-merge template carrying
        // FILTRULE_NO_PREFIXES (`-`/`+`) skips the short-prefix dispatch and
        // consumes each line as a literal exclude (or include for `+`).
        let rules = if descriptor.no_prefixes {
            parse_rules_no_prefixes(
                &content,
                &merge_path,
                descriptor.no_prefixes_include,
                descriptor.cvs_mode,
                descriptor.word_split,
            )
        } else if descriptor.cvs_mode {
            parse_cvs_ignore_tokens(&content)
        } else if descriptor.word_split {
            match parse_rules_word_split(&content, &merge_path) {
                Ok(rules) => rules,
                Err(e) => {
                    self.pop_scopes_at_depth(depth);
                    self.current_depth -= 1;
                    return Err(FilterChainError::Parse {
                        path: merge_path,
                        message: e.to_string(),
                    });
                }
            }
        } else {
            match parse_rules(&content, &merge_path) {
                Ok(rules) => rules,
                Err(e) => {
                    self.pop_scopes_at_depth(depth);
                    self.current_depth -= 1;
                    return Err(FilterChainError::Parse {
                        path: merge_path,
                        message: e.to_string(),
                    });
                }
            }
        };

        if rules.is_empty() {
            return Ok(0);
        }

        let delete_excluded = self.delete_excluded;
        let rules: Vec<FilterRule> = rules
            .into_iter()
            .map(|rule| apply_dir_merge_inherited_side(rule, descriptor))
            .map(|rule| apply_merge_implicit_sender_side(rule, delete_excluded))
            .collect();

        let filter_set = match FilterSet::from_rules(rules) {
            Ok(set) => set,
            Err(e) => {
                self.pop_scopes_at_depth(depth);
                self.current_depth -= 1;
                return Err(FilterChainError::Filter(e));
            }
        };

        if filter_set.is_empty() {
            return Ok(0);
        }

        // An inheriting `:` dir-merge is also registered as a persistent config
        // (see enter_directory), so descendant loads run through the main loop
        // under that config's index. Tag this one-shot scope with the same
        // index so a descendant `!` clears it as one mergelist; unregistered
        // (no-inherit) variants have no cross-directory identity.
        let config_index = self
            .merge_configs
            .iter()
            .position(|c| c.filename() == descriptor.filename);

        // upstream: exclude.c:1248-1254 - `:C` implies FILTRULE_NO_INHERIT,
        // so the loaded rules apply only to the directory containing the
        // outer merge file, not to descendants. Other dir-merge variants
        // preserve their explicit no-inherit setting.
        self.scopes.push(DirScope {
            depth,
            filter_set,
            inherits: !descriptor.no_inherit,
            config_index,
        });
        Ok(1)
    }

    /// Leaves a directory, removing all scopes pushed at the given depth.
    ///
    /// This is called when leaving a directory to restore filter state.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `exclude.c:pop_local_filters()` which restores the filter list
    /// state from before entering the directory.
    pub fn leave_directory(&mut self, guard: DirFilterGuard) {
        self.pop_scopes_at_depth(guard.depth);
        self.restore_cleared_scopes(guard.depth);
        self.current_depth = self.current_depth.saturating_sub(1);
    }

    /// Returns `true` if the chain has no rules at all (global, per-directory
    /// scopes, or per-directory merge configs).
    ///
    /// A chain that carries only a dir-merge directive (e.g. a client's `-F`
    /// `.rsync-filter` transmitted to a server-side receiver) has an empty
    /// `global` rule set and no active `scopes` until a directory is entered,
    /// but it is emphatically not empty: the merge config governs which
    /// destination entries the `--delete` pass may remove. Omitting
    /// `merge_configs` here made such a chain read as empty, so the receiver's
    /// deletion path fell back to the wrong chain and deleted dir-merge-protected
    /// entries. Account for `merge_configs` so a merge-only chain is recognised
    /// as populated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.global.is_empty() && self.scopes.is_empty() && self.merge_configs.is_empty()
    }

    /// Number of active per-directory filter scopes on the stack.
    #[must_use]
    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    /// Directory nesting level relative to the transfer root.
    #[must_use]
    pub fn current_depth(&self) -> usize {
        self.current_depth
    }

    /// Base filter set applied before any per-directory rules.
    #[must_use]
    pub fn global(&self) -> &FilterSet {
        &self.global
    }

    /// Removes all scopes at the specified depth.
    fn pop_scopes_at_depth(&mut self, depth: usize) {
        self.scopes.retain(|scope| scope.depth != depth);
    }

    /// Removes the inherited ancestor scopes of the given merge config,
    /// stashing them for restoration when the clearing directory is left.
    ///
    /// Mirrors upstream `exclude.c:1393-1401`, where a per-directory `!`
    /// clears the mergelist's inherited head. Only scopes with the same
    /// `config_index` are removed, matching `pop_filter_list(listp)` operating
    /// on a single list; scopes belonging to other per-directory merge files
    /// (a different `listp`) are left untouched.
    fn clear_inherited_scopes(&mut self, config_index: usize, depth: usize) {
        let mut i = 0;
        while i < self.scopes.len() {
            if self.scopes[i].config_index == Some(config_index) {
                let scope = self.scopes.remove(i);
                self.cleared_scopes.push(ClearedScope { depth, scope });
            } else {
                i += 1;
            }
        }
    }

    /// Restores scopes cleared by a `!` at the given depth, re-establishing the
    /// pre-descent mergelist for sibling directories.
    ///
    /// Mirrors upstream `exclude.c:pop_local_filters()`. Restored scopes are
    /// re-inserted in stack order (by depth, then config index) so evaluation
    /// still walks innermost-to-outermost correctly.
    fn restore_cleared_scopes(&mut self, depth: usize) {
        if !self.cleared_scopes.iter().any(|c| c.depth == depth) {
            return;
        }
        let mut i = 0;
        while i < self.cleared_scopes.len() {
            if self.cleared_scopes[i].depth == depth {
                let restored = self.cleared_scopes.remove(i).scope;
                self.scopes.push(restored);
            } else {
                i += 1;
            }
        }
        self.scopes.sort_by(|a, b| {
            (a.depth, a.config_index.unwrap_or(usize::MAX))
                .cmp(&(b.depth, b.config_index.unwrap_or(usize::MAX)))
        });
    }

    /// Pushes a pre-built filter set as a per-directory scope.
    ///
    /// This is useful for testing or when rules are obtained from sources
    /// other than merge files (e.g., received over the wire from a remote
    /// sender).
    pub fn push_scope(&mut self, filter_set: FilterSet) -> DirFilterGuard {
        self.current_depth += 1;
        let depth = self.current_depth;
        let is_empty = filter_set.is_empty();
        if !is_empty {
            self.scopes.push(DirScope {
                depth,
                filter_set,
                inherits: true,
                config_index: None,
            });
        }
        DirFilterGuard {
            depth,
            pushed_count: if is_empty { 0 } else { 1 },
        }
    }
}

/// Parses a CVS-style ignore file into exclude rules.
///
/// Splits the content on whitespace and treats each token as an exclude
/// pattern (no `+`/`-` filter prefixes honoured). Blank input produces no
/// rules. This mirrors upstream rsync's CVS-mode merge parsing for
/// `.cvsignore` files registered via `:C` (`exclude.c:1250-1254`).
fn parse_cvs_ignore_tokens(content: &str) -> Vec<FilterRule> {
    content
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(FilterRule::exclude)
        .collect()
}

/// Applies upstream's per-token implicit FILTRULE_SENDER_SIDE flip to a rule
/// expanded from a merge file when `--delete-excluded` is active.
///
/// Mirrors `exclude.c:1324-1332 parse_rule_tok`: the OR fires for every
/// include/exclude rule produced by the parser unless the user explicitly
/// requested a side via the `s` or `r` modifier, in which case the
/// FILTRULES_SIDES bit is already set and the OR is skipped. Merge and
/// dir-merge wrappers themselves are excluded by the FILTRULE_MERGE_FILE /
/// FILTRULE_PERDIR_MERGE check, which oc-rsync handles by extracting those
/// directives before calling this helper.
fn apply_merge_implicit_sender_side(rule: FilterRule, delete_excluded: bool) -> FilterRule {
    if !delete_excluded {
        return rule;
    }
    if !matches!(rule.action(), FilterAction::Include | FilterAction::Exclude) {
        return rule;
    }
    if !(rule.applies_to_sender() && rule.applies_to_receiver()) {
        return rule;
    }
    rule.with_receiver(false)
}

/// Applies a nested dir-merge's inherited side modifier to a loaded rule.
///
/// When a `dir-merge` directive is side-restricted (its own `s`/`r` modifier,
/// or inherited from a side-restricted container per-directory merge), the
/// rules it loads default to that side unless they carry their own. Only
/// two-sided include/exclude rules are adjusted, matching upstream's
/// `FILTRULES_SIDES` inheritance (exclude.c:1293-1303); a rule that already
/// specifies a side keeps it.
fn apply_dir_merge_inherited_side(rule: FilterRule, descriptor: &InlineDirMerge) -> FilterRule {
    if !descriptor.sender_only && !descriptor.receiver_only {
        return rule;
    }
    if !matches!(rule.action(), FilterAction::Include | FilterAction::Exclude) {
        return rule;
    }
    if !(rule.applies_to_sender() && rule.applies_to_receiver()) {
        return rule;
    }
    if descriptor.sender_only {
        rule.with_sides(true, false)
    } else {
        rule.with_sides(false, true)
    }
}

/// Description of a dir-merge directive parsed from another merge file.
///
/// Captures the pieces of the source [`FilterRule`] that drive how the
/// referenced file should be loaded: the filename (e.g. `.cvsignore`),
/// whether to treat its contents as a CVS-style ignore list, and whether
/// the loaded rules should propagate to descendant directories.
#[derive(Clone, Debug)]
struct InlineDirMerge {
    filename: String,
    cvs_mode: bool,
    no_inherit: bool,
    /// Restricts the rules loaded from this merge to the sender side.
    ///
    /// Set from the source `dir-merge` rule's own `s` modifier, then OR-ed
    /// with the side of the containing per-directory merge so a `dir-merge`
    /// nested inside a `:s` merge inherits sender-side. upstream:
    /// `exclude.c:1293-1303` inherits `FILTRULES_SIDES` from the template.
    sender_only: bool,
    /// Receiver-side counterpart of [`Self::sender_only`] (`r` modifier).
    receiver_only: bool,
    /// `-`/`+` modifier (FILTRULE_NO_PREFIXES): each merged line is a literal
    /// pattern rather than a prefixed rule.
    no_prefixes: bool,
    /// Pairs with [`Self::no_prefixes`]: `true` selects the `+` (include)
    /// variant, `false` the `-` (exclude) variant.
    no_prefixes_include: bool,
    /// `/` modifier (FILTRULE_ABS_PATH): anchor merged rules to the transfer
    /// root rather than the merge file's own directory.
    abs_path: bool,
    /// `w` modifier (FILTRULE_WORD_SPLIT): tokenise the merge file on any
    /// whitespace instead of one rule per line.
    word_split: bool,
}

/// Joins a relative path's normal components with `/`, ignoring any leading
/// `./`, `..`, or root components. Produces the module-root-relative directory
/// string used to re-anchor merge-file rules.
fn path_to_forward_slash(path: &Path) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.components() {
        if let Component::Normal(part) = component {
            if let Some(s) = part.to_str() {
                parts.push(s);
            }
        }
    }
    parts.join("/")
}

/// Re-anchors a single rule read from a per-directory merge file.
///
/// upstream: exclude.c:200-228 add_rule - under `XFLG_ANCHORED2ABS`, a rule
/// whose pattern begins with `/` is rewritten to be anchored at the merge
/// file's directory rather than the module root. oc-rsync expresses root
/// anchoring as a leading `/` in the pattern, so `/file1` from merge directory
/// `foo` becomes `/foo/file1`. Rules without a leading `/` (and the root merge
/// file, where `rel_dir` is `None`) are returned unchanged.
fn reanchor_merge_rule(mut rule: FilterRule, rel_dir: Option<&str>) -> FilterRule {
    if let Some(dir) = rel_dir {
        if let Some(rest) = rule.pattern.strip_prefix('/') {
            rule.pattern = format!("/{dir}/{rest}");
        }
    }
    rule
}

/// Splits parsed rules into ordinary entries plus dir-merge descriptors.
///
/// Standard [`FilterSet`] compilation discards [`FilterAction::DirMerge`]
/// rules because they neither match paths nor encode a single decision.
/// Pull them out here so the caller can load each referenced file inline,
/// mirroring upstream's `:` directive that registers a fresh per-directory
/// merge from inside another merge file.
///
/// upstream: exclude.c:1419-1428 - `FILTRULE_PERDIR_MERGE` inside
/// `parse_filter_str()` adds a new per-dir rule rather than expanding the
/// file at parse time.
fn split_dir_merge_rules(rules: Vec<FilterRule>) -> (Vec<FilterRule>, Vec<InlineDirMerge>) {
    let mut keep = Vec::with_capacity(rules.len());
    let mut dir_merges = Vec::new();
    for rule in rules {
        if matches!(rule.action(), FilterAction::DirMerge) {
            let (no_prefixes, no_prefixes_include) = rule.no_prefixes();
            dir_merges.push(InlineDirMerge {
                filename: rule.pattern().to_owned(),
                cvs_mode: rule.is_cvs_mode(),
                no_inherit: rule.is_no_inherit(),
                sender_only: rule.applies_to_sender() && !rule.applies_to_receiver(),
                receiver_only: rule.applies_to_receiver() && !rule.applies_to_sender(),
                no_prefixes,
                no_prefixes_include,
                abs_path: rule.is_abs_path(),
                word_split: rule.is_word_split(),
            });
        } else {
            keep.push(rule);
        }
    }
    (keep, dir_merges)
}
