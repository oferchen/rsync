//! Filter rule handling and `--files-from` path resolution.
//!
//! Receives filter rules from the client (server mode) or applies config-provided
//! rules (client mode), converts wire format to [`FilterSet`] and [`FilterChain`],
//! and resolves `--files-from` filenames to filesystem paths for file list building.
//!
//! Per-directory merge rules (`DirMerge`) are extracted into [`DirMergeConfig`]
//! entries and registered on the [`FilterChain`]. During `walk_path()`, the chain
//! reads merge files from each directory and pushes/pops scoped rules.
//!
//! # Upstream Reference
//!
//! - `main.c:1258` - `recv_filter_list()` in server mode
//! - `flist.c:2240-2264` - `--files-from` filename reading and resolution
//! - `exclude.c:push_local_filters()` - per-directory merge file loading

use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub use ::filters::FilterSet;
use filters::{DirMergeConfig, FilterChain};
use logging::info_log;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};

use crate::role_trailer::error_location;

use super::GeneratorContext;
use ::filters::FilterRule;

/// A resolved `--files-from` entry split into a walk base and a full path.
///
/// Upstream rsync's `flist.c:2316-2330` splits each `--files-from` line on its
/// first `/./` anchor: characters before the anchor name the directory the
/// sender chdirs into, and characters after become the transmitted relative
/// name. Entries without an anchor share the original source argument as
/// their base.
///
/// Carrying the split forward lets `build_file_list_with_base` strip the
/// per-entry `base` so that the wire-side relative name matches what the
/// receiver's `implied_filter_list` (built from the verbatim `--files-from`
/// lines) expects. Without the split, the relative name would still include
/// the anchor prefix and the receiver would reject it as
/// "unrequested file-list name".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesFromEntry {
    /// Effective walk base for this entry. Equal to the source argument for
    /// plain entries; for entries with a `/./` anchor, equal to the source
    /// argument joined with the prefix before the anchor.
    pub base: PathBuf,
    /// Full filesystem path of the entry, i.e. `base` joined with the
    /// transmitted relative name.
    pub path: PathBuf,
    /// True when the original `--files-from` line ended with `/` or with the
    /// `/./` DOTDIR anchor. Upstream `flist.c:2329` flags these as
    /// `SLASH_ENDING_NAME`/`DOTDIR_NAME`, which causes the sender to recurse
    /// into the directory's children even when global `-r` is disabled
    /// (`options.c:2189` clears `recurse` whenever `--files-from` is active).
    pub recurse: bool,
}

/// Splits a sanitized `--files-from` entry on its first `/./` anchor.
///
/// Mirrors upstream `flist.c:2316-2330`: anything before the anchor becomes
/// part of the per-entry walk base; anything after is the transmitted
/// relative name. Entries without an anchor inherit `base_dir` unchanged.
///
/// `raw_trailing_slash` records whether the original `--files-from` line
/// (before sanitisation) ended with `/`, including the `/./` DOTDIR form.
/// Upstream `flist.c:2329` flags those lines as `SLASH_ENDING_NAME` /
/// `DOTDIR_NAME` and recurses into their children even when global `-r` is
/// off (`options.c:2189` clears `recurse` whenever `--files-from` is
/// active), so we propagate the flag onto [`FilesFromEntry::recurse`] for
/// `build_file_list_with_base` to honour.
///
/// A trailing `/.` is treated as an anchor with an empty suffix because
/// [`sanitize_path_keep_dot_dirs`](crate::sanitize_path::sanitize_path_keep_dot_dirs)
/// collapses the trailing slash that upstream's `sanitize_path` preserves
/// (`from/./` → `from/.`). Without this, `from/./` would never match the
/// `/./` substring search and would emit a stray `from/` directory entry
/// instead of promoting `from` to the per-entry walk base.
pub(super) fn split_files_from_entry(
    base_dir: &Path,
    sanitized: &str,
    raw_trailing_slash: bool,
) -> FilesFromEntry {
    // Anchored form: prefix `/./` suffix.
    if let Some(anchor) = sanitized.find("/./") {
        let (head, tail) = sanitized.split_at(anchor);
        // upstream: flist.c:2321 - skip the `/./` separator and any redundant
        // leading slashes on the suffix so `dir/./subdir` and `dir/././subdir`
        // both collapse to a relative name of `subdir`.
        let rest = tail[3..].trim_start_matches('/');
        let base = if head.is_empty() {
            base_dir.to_path_buf()
        } else {
            base_dir.join(head)
        };
        let path = if rest.is_empty() {
            base.clone()
        } else {
            base.join(rest)
        };
        // An empty suffix (e.g. `from/./`) is upstream's DOTDIR_NAME case,
        // which always recurses. Otherwise honour the raw trailing slash.
        let recurse = rest.is_empty() || raw_trailing_slash;
        return FilesFromEntry {
            base,
            path,
            recurse,
        };
    }

    // Trailing-anchor form: prefix `/.` (sanitize stripped the trailing slash
    // upstream would have left). Treat it identically to `prefix/./`.
    if let Some(head) = sanitized.strip_suffix("/.") {
        if !head.is_empty() {
            let base = base_dir.join(head);
            return FilesFromEntry {
                base: base.clone(),
                path: base,
                // Upstream DOTDIR_NAME always recurses.
                recurse: true,
            };
        }
    }

    FilesFromEntry {
        base: base_dir.to_path_buf(),
        path: base_dir.join(sanitized),
        recurse: raw_trailing_slash,
    }
}

impl GeneratorContext {
    /// Receives filter list from client in server mode.
    ///
    /// In server mode, we receive filter rules from the client before building
    /// the file list. In client mode, we already sent filters in mod.rs.
    /// DirMerge rules are extracted and registered on the filter chain for
    /// per-directory merge file processing during the file walk.
    ///
    /// # Upstream Reference
    ///
    /// - Server mode: `recv_filter_list()` at `main.c:1258`
    /// - Client mode: `send_filter_list()` at `main.c:1308` (done in mod.rs)
    pub(super) fn receive_filter_list_if_server<R: Read>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<()> {
        if self.config.connection.client_mode {
            // Client mode: apply filters from config for local file list building.
            // Filter rules were already sent to the daemon in mod.rs.
            // upstream: flist.c:1332 - is_excluded() applied during make_file()
            if !self.config.connection.filter_rules.is_empty() {
                let (filter_set, merge_configs) =
                    self.parse_received_filters(&self.config.connection.filter_rules.clone())?;
                self.filter_chain = FilterChain::new(filter_set);
                for config in merge_configs {
                    self.filter_chain.add_merge_config(config);
                }
            }
            return Ok(());
        }

        // Server mode: read filter list from client (MULTIPLEXED for protocol >= 30)
        let wire_rules = read_filter_list(reader, self.protocol)?;

        // upstream: clientserver.c:rsync_module() - daemon_filter_list is applied
        // on top of client filters. Daemon rules take precedence (prepended).
        let daemon_rules = &self.config.daemon_filter_rules;
        let combined = if daemon_rules.is_empty() {
            wire_rules
        } else if wire_rules.is_empty() {
            daemon_rules.clone()
        } else {
            let mut combined = daemon_rules.clone();
            combined.extend(wire_rules);
            combined
        };

        // Convert wire format to FilterChain
        if !combined.is_empty() {
            let (filter_set, merge_configs) = self.parse_received_filters(&combined)?;
            self.filter_chain = FilterChain::new(filter_set);
            for config in merge_configs {
                self.filter_chain.add_merge_config(config);
            }
        }

        Ok(())
    }

    /// Reads the `--files-from` file list and resolves filenames to walk paths.
    ///
    /// When `files_from_path` is `Some("-")`, the list is read from the protocol
    /// stream (stdin in server mode) using NUL-separated wire format. When it is
    /// `Some(path)` for any other value, the file is opened and read locally.
    ///
    /// Each filename is resolved relative to the first positional argument (the
    /// base source directory). Entries containing a `/./` anchor are split per
    /// upstream `flist.c:2316`: the prefix before the anchor is joined onto the
    /// base to form the entry's effective walk base, and the suffix becomes the
    /// transmitted relative name. Entries without an anchor share `base_dir` as
    /// their effective base. Returns an empty `Vec` when no `--files-from` is
    /// configured, signaling the caller to use the original positional paths.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2262` - `read_line(filesfrom_fd, ...)` reads one name at a time
    /// - `flist.c:2316-2330` - `/./` anchor split for relative-name emission
    /// - `main.c:681-685` - `filesfrom_fd` set to `STDIN_FILENO` for `--files-from=-`
    /// - `io.c:start_filesfrom_forwarding()` - client forwards local file over socket
    pub(super) fn resolve_files_from_paths<R: Read>(
        &self,
        original_paths: &[PathBuf],
        reader: &mut R,
    ) -> io::Result<Vec<FilesFromEntry>> {
        let files_from_path = match &self.config.file_selection.files_from_path {
            Some(path) => path.clone(),
            None => return Ok(Vec::new()),
        };

        // Determine base directory: use the first positional arg (source dir).
        // upstream: flist.c:2240-2244 - change_dir(argv[0]) before reading filenames.
        let base_dir = original_paths
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));

        let filenames = if files_from_path == "-" {
            // Read from protocol stream (stdin). The client forwards the file
            // list as NUL-separated entries with a double-NUL terminator.
            // upstream: main.c:681 - filesfrom_fd = STDIN_FILENO
            // upstream: io.c:read_line(RL_CONVERT) - wire bytes are UTF-8 and
            // must be transcoded to the local charset via ic_recv when
            // protect_args && --iconv are both in effect (compat.c:799-806).
            protocol::read_files_from_stream(reader, self.config.connection.iconv.as_ref())?
        } else {
            // Read from a local file on the server.
            // upstream: main.c:675-679 - open(files_from, O_RDONLY)
            // The file lives in the server's local charset, so no wire iconv
            // applies - read it as-is, mirroring upstream's omission of
            // RL_CONVERT for the local-file fd.
            let from0 = self.config.file_selection.from0;
            read_files_from_local_path(&files_from_path, from0)?
        };

        // upstream: flist.c:2240-2264 - chdir to argv[0] then read relative
        // filenames. Each entry's effective base is base_dir plus any prefix
        // before its `/./` anchor (upstream's `dir` variable in flist.c:2316),
        // so the wire-side relative name is the path after the anchor.
        let mut resolved = Vec::with_capacity(filenames.len());
        for name in &filenames {
            if name.is_empty() {
                continue;
            }
            // upstream: flist.c:2264 - sanitize_path(fbuf, fbuf, "", 0, SP_KEEP_DOT_DIRS)
            // Always sanitize files_from entries to prevent directory traversal.
            // This collapses ".." components and strips leading "/" to confine
            // paths within the transfer root. `SP_KEEP_DOT_DIRS` preserves the
            // `/./` anchor so the split below mirrors upstream exactly.
            let sanitized = crate::sanitize_path::sanitize_path_keep_dot_dirs(name);
            // upstream: flist.c:2329 - a raw trailing slash flags the entry
            // as SLASH_ENDING_NAME, which forces recursion even when global
            // `-r` is off. Capture it from the original (pre-sanitisation)
            // line so the split can propagate the flag.
            let raw_trailing_slash = name.ends_with('/');
            resolved.push(split_files_from_entry(
                &base_dir,
                &sanitized,
                raw_trailing_slash,
            ));
        }

        info_log!(
            Flist,
            1,
            "read {} filenames from --files-from source",
            resolved.len()
        );

        Ok(resolved)
    }

    /// Converts wire format rules to a global `FilterSet` and per-directory `DirMergeConfig` list.
    ///
    /// Maps the wire protocol representation to the filters crate's types. Include,
    /// Exclude, Protect, Risk, and Clear rules are compiled into a single `FilterSet`.
    /// DirMerge rules are extracted into `DirMergeConfig` entries for registration
    /// on the `FilterChain`. Merge rules (non-directory) are skipped since their
    /// contents are pre-expanded by the client before transmission.
    ///
    /// # Upstream Reference
    ///
    /// - `exclude.c:parse_filter_file()` - filter list construction
    /// - `exclude.c:push_local_filters()` - DirMerge rules drive per-dir scanning
    pub(super) fn parse_received_filters(
        &self,
        wire_rules: &[FilterRuleWireFormat],
    ) -> io::Result<(FilterSet, Vec<DirMergeConfig>)> {
        let mut rules = Vec::with_capacity(wire_rules.len());
        let mut merge_configs = Vec::new();

        for wire_rule in wire_rules {
            // The wire format stores the bare pattern in `pattern` and carries
            // the anchored / directory-only modifiers as separate flags. The
            // `filters` crate, however, encodes those modifiers as leading and
            // trailing `/` in the pattern string itself, so reattach them
            // before constructing the rule. Without this, `--include='*/'`
            // would be received as the plain pattern `*` and lose its
            // directory-only semantics, leading to a subsequent `--exclude='*'`
            // swallowing directories that the user intended to traverse.
            //
            // upstream: exclude.c:get_rule_prefix() encodes anchored as the
            // `/` modifier and directory-only as a trailing slash on the
            // pattern body.
            let reconstructed_pattern = reconstruct_pattern(wire_rule);
            let mut rule = match wire_rule.rule_type {
                RuleType::Include => FilterRule::include(reconstructed_pattern),
                RuleType::Exclude => FilterRule::exclude(reconstructed_pattern),
                RuleType::Protect => FilterRule::protect(reconstructed_pattern),
                RuleType::Risk => FilterRule::risk(reconstructed_pattern),
                RuleType::Clear => {
                    // Clear rule removes all previous rules
                    rules.push(
                        FilterRule::clear()
                            .with_sides(wire_rule.sender_side, wire_rule.receiver_side),
                    );
                    continue;
                }
                RuleType::DirMerge => {
                    // upstream: exclude.c - dir-merge rules register a per-directory
                    // merge file that is read during walk_path(). The FilterChain
                    // handles reading and scoping.
                    let config = wire_rule_to_dir_merge_config(wire_rule);
                    merge_configs.push(config);
                    continue;
                }
                RuleType::Merge => {
                    // Merge (non-directory) rules are pre-expanded by the client
                    // before transmission - their contents are inlined as regular
                    // include/exclude rules. Skip the merge directive itself.
                    continue;
                }
            };

            // Apply modifiers
            if wire_rule.sender_side || wire_rule.receiver_side {
                rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
            }

            if wire_rule.perishable {
                rule = rule.with_perishable(true);
            }

            if wire_rule.xattr_only {
                rule = rule.with_xattr_only(true);
            }

            if wire_rule.negate {
                rule = rule.with_negate(true);
            }

            // Note: no_inherit, cvs_exclude, word_split, exclude_from_merge
            // are pattern modifiers handled by the filters crate during compilation.

            rules.push(rule);
        }

        let filter_set = FilterSet::from_rules(rules).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "filter error: {e} {}{}",
                    error_location!(),
                    crate::role_trailer::sender()
                ),
            )
        })?;

        Ok((filter_set, merge_configs))
    }
}

/// Reassembles the pattern body with leading/trailing `/` modifiers re-applied.
///
/// Wire format separates the anchored (`/`) and directory-only (`/`) modifiers
/// from the pattern body, while the `filters` crate expects them embedded in
/// the pattern string. This restores them so downstream rule compilation
/// observes the user's original intent.
fn reconstruct_pattern(wire_rule: &FilterRuleWireFormat) -> String {
    let mut pattern = String::with_capacity(wire_rule.pattern.len() + 2);
    if wire_rule.anchored && !wire_rule.pattern.starts_with('/') {
        pattern.push('/');
    }
    pattern.push_str(&wire_rule.pattern);
    if wire_rule.directory_only && !pattern.ends_with('/') {
        pattern.push('/');
    }
    pattern
}

/// Converts a wire-format DirMerge rule into a `DirMergeConfig`.
///
/// Maps wire protocol modifier flags to the corresponding `DirMergeConfig`
/// builder methods. The pattern field contains the merge filename.
///
/// # Upstream Reference
///
/// - `exclude.c:parse_filter_str()` - modifier flag parsing for dir-merge rules
fn wire_rule_to_dir_merge_config(wire_rule: &FilterRuleWireFormat) -> DirMergeConfig {
    // upstream: exclude.c - a leading '/' on the merge filename means the
    // file is only looked for in the transfer root directory (anchor_root).
    // Strip the '/' so Path::join() produces a relative path.
    let (filename, anchor_root) = match wire_rule.pattern.strip_prefix('/') {
        Some(stripped) => (stripped, true),
        None => (wire_rule.pattern.as_str(), false),
    };
    let mut config = DirMergeConfig::new(filename);
    if anchor_root {
        config = config.with_anchor_root(true);
    }

    // `n` modifier: no-inherit (rules apply only in the containing directory)
    if wire_rule.no_inherit {
        config = config.with_inherit(false);
    }

    // `e` modifier: exclude the merge file itself from transfer
    if wire_rule.exclude_from_merge {
        config = config.with_exclude_self(true);
    }

    // `s` modifier: sender-side only
    if wire_rule.sender_side {
        config = config.with_sender_only(true);
    }

    // `r` modifier: receiver-side only
    if wire_rule.receiver_side {
        config = config.with_receiver_only(true);
    }

    // `p` modifier: perishable
    if wire_rule.perishable {
        config = config.with_perishable(true);
    }

    config
}

/// Reads a `--files-from` list from a local file path on the server.
///
/// When the server's `--files-from` points to a file (not stdin/`-`), this
/// opens and reads the file using the standard line-based or NUL-based format.
///
/// # Upstream Reference
///
/// - `main.c:675-679` - `open(files_from, O_RDONLY)` for local file
/// - `flist.c:2262` - `read_line(filesfrom_fd, ...)` reads lines
pub(super) fn read_files_from_local_path(path: &str, from0: bool) -> io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);

    if from0 {
        // NUL-delimited: use the wire format reader which handles NUL separators.
        // The local file is already in the server's local charset; upstream
        // reads it without RL_CONVERT (compat.c:799-806 only sets
        // filesfrom_convert when the file is being forwarded over the wire).
        protocol::read_files_from_stream(&mut reader, None)
    } else {
        // Line-delimited: read lines, skip comments and empty lines.
        let mut filenames = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = io::BufRead::read_line(&mut reader, &mut line)?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                continue;
            }
            // upstream: flist.c:2266 - comments with '#' or ';' prefix
            // only when not using --from0
            if trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            filenames.push(trimmed.to_owned());
        }
        Ok(filenames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::filters::RuleType;

    fn make_dir_merge_wire_rule(pattern: &str) -> FilterRuleWireFormat {
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: pattern.to_owned(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: false,
            perishable: false,
            negate: false,
        }
    }

    #[test]
    fn wire_rule_to_dir_merge_config_strips_leading_slash() {
        let wire_rule = make_dir_merge_wire_rule("/.rsync-filter");
        let config = wire_rule_to_dir_merge_config(&wire_rule);
        assert_eq!(config.filename(), ".rsync-filter");
    }

    #[test]
    fn wire_rule_to_dir_merge_config_no_slash() {
        let wire_rule = make_dir_merge_wire_rule(".rsync-filter");
        let config = wire_rule_to_dir_merge_config(&wire_rule);
        assert_eq!(config.filename(), ".rsync-filter");
    }

    #[test]
    fn split_files_from_entry_without_anchor_inherits_base() {
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "dir/file.txt", false);
        assert_eq!(split.base, PathBuf::from("/src"));
        assert_eq!(split.path, PathBuf::from("/src/dir/file.txt"));
        assert!(!split.recurse);
    }

    #[test]
    fn split_files_from_entry_with_anchor_promotes_prefix_to_base() {
        // UTS-21.REOPEN regression: `from/./dir/subdir` must split so that
        // the wire-side relative name (path.strip_prefix(base)) is just
        // `dir/subdir`. Otherwise upstream's `implied_filter_list` check
        // (flist.c:998) rejects `from/dir/subdir` as "unrequested".
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "from/./dir/subdir", false);
        assert_eq!(split.base, PathBuf::from("/src/from"));
        assert_eq!(split.path, PathBuf::from("/src/from/dir/subdir"));
        assert!(!split.recurse);
        let rel = split.path.strip_prefix(&split.base).unwrap();
        assert_eq!(rel, std::path::Path::new("dir/subdir"));
    }

    #[test]
    fn split_files_from_entry_with_trailing_anchor_keeps_base_as_path() {
        // upstream: flist.c:2321-2324 - `from/./` (trailing `/.`) emits the
        // anchor directory itself, with the relative name collapsing to `.`,
        // and is always recursed into.
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "from/./", true);
        assert_eq!(split.base, PathBuf::from("/src/from"));
        assert_eq!(split.path, PathBuf::from("/src/from"));
        assert!(split.recurse);
    }

    #[test]
    fn split_files_from_entry_with_trailing_dot_after_sanitize_keeps_base_as_path() {
        // UTS-21.REOPEN: `sanitize_path_keep_dot_dirs("from/./")` returns
        // `"from/."` because our sanitizer strips the trailing slash that
        // upstream preserves. Without trailing-dot handling, the split would
        // miss the anchor and emit a `from/` directory entry instead of
        // promoting `from` to the walk base.
        let sanitized = crate::sanitize_path::sanitize_path_keep_dot_dirs("from/./");
        assert_eq!(sanitized, "from/.");
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, &sanitized, true);
        assert_eq!(split.base, PathBuf::from("/src/from"));
        assert_eq!(split.path, PathBuf::from("/src/from"));
        assert!(split.recurse);
    }

    #[test]
    fn split_files_from_entry_with_trailing_slash_recurses() {
        // UTS-21.REOPEN: `from/./dir/subdir/subsubdir2/` (trailing slash) is
        // upstream's `SLASH_ENDING_NAME` case, which forces recursion into
        // the named directory even though `--files-from` clears the global
        // `-r` flag.
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "from/./dir/subdir/subsubdir2", true);
        assert_eq!(split.base, PathBuf::from("/src/from"));
        assert_eq!(split.path, PathBuf::from("/src/from/dir/subdir/subsubdir2"));
        assert!(split.recurse);
    }

    #[test]
    fn split_files_from_entry_collapses_redundant_separator_slashes() {
        // `dir/././sub` should behave like `dir/./sub`: head is `dir`, rest is `sub`.
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "dir/././sub", false);
        assert_eq!(split.base, PathBuf::from("/src/dir"));
        // The second `./` is part of `rest` and joins as `./sub`; PathBuf
        // join keeps it as a no-op fs component.
        assert_eq!(split.path, PathBuf::from("/src/dir/./sub"));
        // The wire-relative name is computed via `Path::components` which
        // normalizes the leading `.`, so the receiver sees `sub`.
        let rel = split.path.strip_prefix(&split.base).unwrap();
        let comps: Vec<_> = rel
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(comps, vec!["sub".to_owned()]);
    }
}
