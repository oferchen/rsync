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
//! - `main.c:1276` - `recv_filter_list()` in server mode
//! - `flist.c:2240-2264` - `--files-from` filename reading and resolution
//! - `exclude.c:push_local_filters()` - per-directory merge file loading

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub use ::filters::FilterSet;
use filters::{DirMergeConfig, FilterChain, cvs_exclusion_rules};
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
    /// True when the transmitted relative name (after any `/./` split) begins
    /// with a leading `./` anchor followed by more path, mirroring upstream's
    /// `implied_dot_dir` trigger (`flist.c:2368`:
    /// `*fn == '.' && fn[1] == '/' && fn[2]`). In `--relative` mode this makes
    /// the sender emit a single transfer-root `.` entry with `FLAG_IMPLIED_DIR`
    /// (`flist.c:2417-2419`). Plain entries without a leading `./` leave it
    /// unset so no root `.` is emitted.
    pub implied_dot: bool,
}

/// Returns true when a transmitted relative name carries a leading `./`
/// anchor with content after it, mirroring upstream's `implied_dot_dir`
/// detection at `flist.c:2368` (`*fn == '.' && fn[1] == '/' && fn[2]`). The
/// third-byte requirement excludes a bare `.` or `./`.
fn has_leading_dot_anchor(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() > 2 && b[0] == b'.' && b[1] == b'/'
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
/// off (`options.c:2207` clears `recurse` whenever `--files-from` is
/// active), so we propagate the flag onto [`FilesFromEntry::recurse`] for
/// `build_file_list_with_base` to honour.
///
/// A trailing `/.` is treated as an anchor with an empty suffix because
/// [`sanitize_path_keep_dot_dirs`](crate::sanitize_path::sanitize_path_keep_dot_dirs)
/// collapses the trailing slash that upstream's `sanitize_path` preserves
/// (`from/./` → `from/.`). Without this, `from/./` would never match the
/// `/./` substring search and would emit a stray `from/` directory entry
/// instead of promoting `from` to the per-entry walk base.
///
/// `relative_paths` selects the upstream split branch. In relative mode
/// (`flist.c:2385-2400`) the entry is split on its first `/./` anchor as
/// above. Under `--no-relative` (`relative_paths == 0`, `flist.c:2338-2349`)
/// upstream instead splits on the entry's LAST `/`: the parent becomes the
/// chdir target (walk base) and only the basename is transmitted, so nested
/// entries FLATTEN (`sub/file` transmits as `file`, no implied `sub` dir).
/// The `/./` anchor is relative-only and is not honoured under `--no-relative`.
pub(super) fn split_files_from_entry(
    base_dir: &Path,
    sanitized: &str,
    raw_trailing_slash: bool,
    relative_paths: bool,
) -> FilesFromEntry {
    // upstream: flist.c:2338-2349 - `if (!relative_paths) { p = strrchr(fbuf,
    // '/'); ... dir = fbuf; fn = p + 1; }`. Non-relative mode drops every
    // leading path component: the walk base absorbs the parent directory and
    // the transmitted name is the trailing basename. No implied parent dirs.
    if !relative_paths {
        let trimmed = sanitized.trim_end_matches('/');
        if raw_trailing_slash {
            // upstream: flist.c:2312-2322 - a trailing `/` turns `X/` into the
            // DOTDIR `X/.`; the strrchr split then makes the WHOLE directory the
            // chdir target (`dir = X`, `fn = "."`) and recurses, so the entry's
            // contents flatten into the transfer root (`sub/` sends `file`,
            // `deep`, `deep/x` - not `sub/...`). Recursion is forced regardless
            // of the global `-r` flag (SLASH_ENDING_NAME / DOTDIR_NAME).
            let base = if trimmed.is_empty() {
                base_dir.to_path_buf()
            } else {
                base_dir.join(trimmed)
            };
            return FilesFromEntry {
                base: base.clone(),
                path: base,
                recurse: true,
                // upstream: flist.c:2367 - `implied_dot_dir` is set only in the
                // relative-mode branch; non-relative entries never trip it.
                implied_dot: false,
            };
        }
        let (head, fname) = match trimmed.rsplit_once('/') {
            Some((head, fname)) => (head, fname),
            None => ("", trimmed),
        };
        let base = if head.is_empty() {
            base_dir.to_path_buf()
        } else {
            base_dir.join(head)
        };
        let path = if fname.is_empty() {
            base.clone()
        } else {
            base.join(fname)
        };
        return FilesFromEntry {
            base,
            path,
            recurse: false,
            // upstream: flist.c:2367 - `implied_dot_dir` is relative-mode only.
            implied_dot: false,
        };
    }

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
        // upstream: flist.c:2359-2368 - after the `/./` split, `fn` is `rest`;
        // a further leading `./` on it still trips `implied_dot_dir`.
        let implied_dot = has_leading_dot_anchor(rest);
        return FilesFromEntry {
            base,
            path,
            recurse,
            implied_dot,
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
                // The transmitted name is a bare `.`, so no implied root dir.
                implied_dot: false,
            };
        }
    }

    FilesFromEntry {
        base: base_dir.to_path_buf(),
        path: base_dir.join(sanitized),
        recurse: raw_trailing_slash,
        // No `/./` split: the whole sanitized name is the transmitted `fn`.
        implied_dot: has_leading_dot_anchor(sanitized),
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
    /// - Server mode: `recv_filter_list()` at `main.c:1276`
    /// - Client mode: `send_filter_list()` at `main.c:1326` (done in mod.rs)
    pub(super) fn receive_filter_list_if_server<R: Read>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<()> {
        if self.config.connection.client_mode {
            // Client mode: apply filters from config for local file list building.
            // Filter rules were already sent to the daemon in mod.rs.
            // upstream: flist.c:1360 - is_excluded() applied during make_file()
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
    /// - `flist.c:2297` - `read_line(filesfrom_fd, ...)` reads one name at a time
    /// - `flist.c:2316-2330` - `/./` anchor split for relative-name emission
    /// - `main.c:681-685` - `filesfrom_fd` set to `STDIN_FILENO` for `--files-from=-`
    /// - `io.c:start_filesfrom_forwarding()` - client forwards local file over socket
    pub fn resolve_files_from_paths<R: Read>(
        &self,
        original_paths: &[PathBuf],
        reader: &mut R,
    ) -> io::Result<Vec<FilesFromEntry>> {
        let files_from_path = match &self.config.file_selection.files_from_path {
            Some(path) => path.clone(),
            None => return Ok(Vec::new()),
        };

        // Determine base directory: use the first positional arg (source dir).
        // upstream: flist.c:2275-2279 - change_dir(argv[0]) before reading filenames.
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
            // UTS-V3.D defensive timeout: cap the receiver-side wait at
            // FILES_FROM_RECV_DEFAULT_DEADLINE (30 s) so a client-side
            // resolver regression surfaces as ETIMEDOUT instead of the 300 s
            // testsuite hang documented in
            // docs/design/uts-v3-d-files-from-hang-audit.md.
            protocol::read_files_from_stream_with_deadline(
                reader,
                self.config.connection.iconv.as_ref(),
                protocol::FILES_FROM_RECV_DEFAULT_DEADLINE,
            )?
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
            // upstream: flist.c:2299 - sanitize_path(fbuf, fbuf, "", 0, SP_KEEP_DOT_DIRS)
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
                self.config.flags.relative,
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
        // upstream: exclude.c:1350 - get_cvs_excludes() flags rules as
        // FILTRULE_PERISHABLE only when protocol_version >= 30.
        let cvs_perishable = self.protocol.as_u8() >= 30;

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
                RuleType::Exclude => {
                    // upstream: exclude.c:1441-1443 - a `-C` rule (cvs_exclude
                    // with no pattern) triggers get_cvs_excludes() on the
                    // local side, populating the filter list with the
                    // default CVS-ignore patterns, `$HOME/.cvsignore`, and
                    // `$CVSIGNORE`. Without expansion here, the bare empty
                    // pattern's synthetic `**/` matcher would exclude every
                    // top-level entry on the sender, defeating the whole
                    // file list.
                    if wire_rule.cvs_exclude && wire_rule.pattern.is_empty() {
                        rules.extend(cvs_default_exclude_rules(cvs_perishable));
                        continue;
                    }
                    FilterRule::exclude(reconstructed_pattern)
                }
                RuleType::Protect => FilterRule::protect(reconstructed_pattern),
                RuleType::Risk => FilterRule::risk(reconstructed_pattern),
                RuleType::Clear => {
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
                    let config = wire_rule_to_dir_merge_config(wire_rule, cvs_perishable);
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

            // Note: no_inherit, word_split, exclude_from_merge are pattern
            // modifiers handled by the filters crate during compilation.
            // The `C` (cvs_exclude) modifier on Exclude rules is expanded
            // above into the equivalent CVS-ignore default rules.

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
/// When the wire rule carries the `C` modifier (CVS-mode dir-merge, e.g.
/// `:C .cvsignore`), upstream `exclude.c:1248-1254` implicitly sets
/// `NO_PREFIXES | WORD_SPLIT | NO_INHERIT | CVS_IGNORE`. Mirror that by
/// switching the config to CVS-mode parsing and disabling inheritance, and
/// mark the rules as perishable when the negotiated protocol is >= 30.
///
/// # Upstream Reference
///
/// - `exclude.c:parse_filter_str()` - modifier flag parsing for dir-merge rules
/// - `exclude.c:1248-1254` - `C` modifier implies word-split + no-inherit + CVS-mode
fn wire_rule_to_dir_merge_config(
    wire_rule: &FilterRuleWireFormat,
    cvs_perishable: bool,
) -> DirMergeConfig {
    // upstream: exclude.c:1404-1408 - when a merge or dir-merge rule carries
    // FILTRULE_CVS_IGNORE (the `C` modifier, e.g. `-f:C` sent as `:C` on the
    // wire) and arrives with an empty pattern, upstream substitutes
    // ".cvsignore" as the default filename. Without this fallback the
    // resulting `DirMergeConfig` would have an empty filename, which causes
    // `enter_directory()` to look up the directory itself instead of any
    // `.cvsignore` it contains, dropping CVS-style ignores entirely.
    let pattern: &str = if wire_rule.cvs_exclude && wire_rule.pattern.is_empty() {
        ".cvsignore"
    } else {
        wire_rule.pattern.as_str()
    };

    // upstream: exclude.c - a leading '/' on the merge filename means the
    // file is only looked for in the transfer root directory (anchor_root).
    // Strip the '/' so Path::join() produces a relative path.
    let (filename, anchor_root) = match pattern.strip_prefix('/') {
        Some(stripped) => (stripped, true),
        None => (pattern, false),
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

    // `w` modifier: FILTRULE_WORD_SPLIT. The per-directory merge file is
    // tokenised on any whitespace, with each token parsed as its own rule
    // (exclude.c:1279-1283, tokenised at exclude.c:1499). Without carrying
    // this on the sender's merge load, a `:w .filt` whose file holds
    // whitespace-separated patterns is parsed one-rule-per-line, so a line
    // like `-_a -_b -_c` collapses into a single malformed rule instead of
    // three. Mirror the local parser, which already word-splits.
    if wire_rule.word_split {
        config = config.with_word_split(true);
    }

    // `C` modifier: CVS-style ignore list. Mirror upstream's implicit
    // NO_PREFIXES | WORD_SPLIT | NO_INHERIT | CVS_IGNORE so that the dir
    // merge filename's contents are parsed as whitespace-separated exclude
    // tokens. Without this, `.cvsignore` lines like `one-in-one-out` would
    // be rejected by the standard merge parser and abort the sender walk.
    // Upstream's `:C` does NOT imply FILTRULE_EXCLUDE_SELF - only the
    // explicit `e` modifier does (exclude.c:1256-1260); leave the existing
    // `e` handling above to drive `excludes_self`.
    if wire_rule.cvs_exclude {
        config = config.with_cvs_mode(true).with_inherit(false);
        if cvs_perishable {
            config = config.with_perishable(true);
        }
    }

    // `-`/`+` modifier: FILTRULE_NO_PREFIXES. Per-dir merge file lines are
    // consumed as literal patterns; the short-prefix dispatch is skipped.
    // upstream: exclude.c:1116-1133 parse_rule_tok.
    if wire_rule.no_prefixes {
        config = config.with_no_prefixes(true, wire_rule.no_prefixes_include);
    }

    config
}

/// Builds the local CVS-ignore exclude list for a `-C` wire rule.
///
/// Mirrors upstream `exclude.c:get_cvs_excludes()`: the built-in
/// `DEFAULT_CVSIGNORE` patterns, then any tokens from `$HOME/.cvsignore`,
/// then any tokens from `$CVSIGNORE`. Each entry becomes an exclude rule
/// with `perishable=true` when the negotiated protocol is >= 30.
///
/// # Upstream Reference
///
/// - `exclude.c:1340-1358 get_cvs_excludes()` - default patterns, HOME/.cvsignore, env.
fn cvs_default_exclude_rules(perishable: bool) -> Vec<FilterRule> {
    let mut out: Vec<FilterRule> = cvs_exclusion_rules(perishable).collect();

    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        let path = Path::new(&home).join(".cvsignore");
        if let Ok(contents) = fs::read(&path) {
            let text = String::from_utf8_lossy(&contents).into_owned();
            append_cvsignore_tokens(&mut out, &text, perishable);
        }
    }

    if let Some(value) = env::var_os("CVSIGNORE").filter(|value| !value.is_empty()) {
        let text = value.to_string_lossy().into_owned();
        append_cvsignore_tokens(&mut out, &text, perishable);
    }

    out
}

/// Splits a CVS-ignore source on whitespace and appends an exclude rule per
/// token, mirroring upstream's word-split parsing of `$CVSIGNORE` and
/// `$HOME/.cvsignore` in `exclude.c:get_cvs_excludes()`.
fn append_cvsignore_tokens(rules: &mut Vec<FilterRule>, source: &str, perishable: bool) {
    for token in source.split_whitespace() {
        if token.is_empty() {
            continue;
        }
        let mut rule = FilterRule::exclude(token.to_owned());
        if perishable {
            rule = rule.with_perishable(true);
        }
        rules.push(rule);
    }
}

/// Reads a `--files-from` list from a local file path on the server.
///
/// When the server's `--files-from` points to a file (not stdin/`-`), this
/// opens and reads the file using the standard line-based or NUL-based format.
///
/// # Upstream Reference
///
/// - `main.c:675-679` - `open(files_from, O_RDONLY)` for local file
/// - `flist.c:2297` - `read_line(filesfrom_fd, ...)` reads lines
pub(super) fn read_files_from_local_path(path: &str, from0: bool) -> io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);

    if from0 {
        // NUL-delimited: use the wire format reader which handles NUL separators.
        // The local file is already in the server's local charset; upstream
        // reads it without RL_CONVERT (compat.c:799-806 only sets
        // filesfrom_convert when the file is being forwarded over the wire).
        //
        // upstream: flist.c:2249 sets RL_DUMP_COMMENTS independent of eol_nulls
        // (it is gated only on reading_remotely), and io.c:1276 read_line()
        // strips leading '#'/';' comment lines even with NUL delimiters. A
        // local file open is not "reading remotely", so comments are stripped.
        let mut filenames = protocol::read_files_from_stream(&mut reader, None)?;
        filenames.retain(|name| !name.starts_with('#') && !name.starts_with(';'));
        Ok(filenames)
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
            // upstream: io.c:1276 - RL_DUMP_COMMENTS strips leading '#'/';'
            // comment lines for local files (flist.c:2249, reading_remotely
            // false), regardless of eol_nulls.
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
            ..FilterRuleWireFormat::default()
        }
    }

    #[test]
    fn wire_rule_to_dir_merge_config_strips_leading_slash() {
        let wire_rule = make_dir_merge_wire_rule("/.rsync-filter");
        let config = wire_rule_to_dir_merge_config(&wire_rule, true);
        assert_eq!(config.filename(), ".rsync-filter");
    }

    #[test]
    fn wire_rule_to_dir_merge_config_no_slash() {
        let wire_rule = make_dir_merge_wire_rule(".rsync-filter");
        let config = wire_rule_to_dir_merge_config(&wire_rule, true);
        assert_eq!(config.filename(), ".rsync-filter");
    }

    /// A `:w .filt` dir-merge arrives over the wire with `word_split=true`.
    /// The remote sender must carry that onto the `DirMergeConfig` so the
    /// per-directory file is tokenised on whitespace, matching the local path.
    /// Without this the sender parses the merge file one-rule-per-line and a
    /// whitespace-separated rule list collapses into a single malformed rule.
    #[test]
    fn wire_rule_to_dir_merge_config_word_split_flag_sets_word_split() {
        let mut wire_rule = make_dir_merge_wire_rule(".filt");
        wire_rule.word_split = true;
        let config = wire_rule_to_dir_merge_config(&wire_rule, true);
        assert!(config.word_split());
        // A plain `:w` must not imply CVS-mode or no-prefixes.
        assert!(!config.cvs_mode());
        assert!(!config.no_prefixes());
    }

    #[test]
    fn wire_rule_to_dir_merge_config_word_split_combines_with_no_prefixes() {
        let mut wire_rule = make_dir_merge_wire_rule(".filt");
        wire_rule.word_split = true;
        wire_rule.no_prefixes = true;
        let config = wire_rule_to_dir_merge_config(&wire_rule, true);
        assert!(config.word_split());
        assert!(config.no_prefixes());
    }

    /// `:C .cvsignore` (FILTRULE_CVS_IGNORE on a DirMerge) must switch the
    /// `DirMergeConfig` into CVS-mode so the chain parses each whitespace
    /// token in `.cvsignore` as an exclude rule. Without this, lines like
    /// `one-in-one-out` fail standard merge parsing and abort the walk.
    /// Upstream's `:C` does NOT exclude the merge file itself; only the
    /// explicit `e` modifier does.
    #[test]
    fn wire_rule_to_dir_merge_config_cvs_flag_enables_cvs_mode() {
        let mut wire_rule = make_dir_merge_wire_rule(".cvsignore");
        wire_rule.cvs_exclude = true;
        let config = wire_rule_to_dir_merge_config(&wire_rule, true);
        assert!(config.cvs_mode());
        assert!(!config.inherits());
        assert!(!config.excludes_self());
    }

    /// `-f:C` over remote shell arrives as a DirMerge wire rule with the `C`
    /// modifier set and an empty pattern. Upstream `exclude.c:1404-1408`
    /// substitutes `.cvsignore` as the default filename so the receiver knows
    /// which per-directory file to consult. Without this fallback the merge
    /// filename would be empty, causing the chain to stat each directory's
    /// own inode and skip `.cvsignore` entirely.
    #[test]
    fn wire_rule_to_dir_merge_config_empty_pattern_defaults_to_cvsignore() {
        let mut wire_rule = make_dir_merge_wire_rule("");
        wire_rule.cvs_exclude = true;
        let config = wire_rule_to_dir_merge_config(&wire_rule, true);
        assert_eq!(config.filename(), ".cvsignore");
        assert!(config.cvs_mode());
        assert!(!config.inherits());
    }

    #[test]
    fn split_files_from_entry_without_anchor_inherits_base() {
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "dir/file.txt", false, true);
        assert_eq!(split.base, PathBuf::from("/src"));
        assert_eq!(split.path, PathBuf::from("/src/dir/file.txt"));
        assert!(!split.recurse);
    }

    #[test]
    fn split_files_from_entry_no_relative_flattens_to_basename() {
        // Task #292: upstream flist.c:2338-2349 - under --no-relative the entry
        // splits on its LAST `/`, so the walk base absorbs every parent
        // component and only the basename is transmitted. `sub/file` must
        // resolve to base `/src/sub`, path `/src/sub/file`, wire name `file` -
        // no implied `sub` directory. An upstream receiver rejects an
        // unrequested intermediate `sub` with exit 4.
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "sub/file", false, false);
        assert_eq!(split.base, PathBuf::from("/src/sub"));
        assert_eq!(split.path, PathBuf::from("/src/sub/file"));
        assert_eq!(
            split.path.strip_prefix(&split.base).unwrap(),
            Path::new("file")
        );
        assert!(!split.recurse);

        // Deeper nesting still flattens to the trailing basename (last `/`).
        let deep = split_files_from_entry(&base, "a/b/c/file", false, false);
        assert_eq!(deep.base, PathBuf::from("/src/a/b/c"));
        assert_eq!(
            deep.path.strip_prefix(&deep.base).unwrap(),
            Path::new("file")
        );

        // A top-level entry keeps the source argument as its base.
        let top = split_files_from_entry(&base, "file", false, false);
        assert_eq!(top.base, PathBuf::from("/src"));
        assert_eq!(top.path, PathBuf::from("/src/file"));

        // The `/./` anchor is relative-only; under --no-relative it is a
        // literal component and the split still takes the last `/`.
        let anchored = split_files_from_entry(&base, "from/./dir/file", false, false);
        assert_eq!(anchored.base, PathBuf::from("/src/from/./dir"));
        assert_eq!(
            anchored.path.strip_prefix(&anchored.base).unwrap(),
            Path::new("file")
        );

        // A trailing slash names a directory whose contents flatten into the
        // transfer root: base is the whole dir, path == base (transmit `.`),
        // and recursion is forced (upstream DOTDIR / SLASH_ENDING_NAME).
        let dir = split_files_from_entry(&base, "sub", true, false);
        assert_eq!(dir.base, PathBuf::from("/src/sub"));
        assert_eq!(dir.path, PathBuf::from("/src/sub"));
        assert!(dir.recurse);

        // A plain directory entry (no trailing slash) keeps the parent as base,
        // transmits the basename, and is NOT recursed (files-from clears `-r`).
        let plain_dir = split_files_from_entry(&base, "sub", false, false);
        assert_eq!(plain_dir.base, PathBuf::from("/src"));
        assert_eq!(plain_dir.path, PathBuf::from("/src/sub"));
        assert!(!plain_dir.recurse);
    }

    #[test]
    fn split_files_from_entry_with_anchor_promotes_prefix_to_base() {
        // UTS-21.REOPEN regression: `from/./dir/subdir` must split so that
        // the wire-side relative name (path.strip_prefix(base)) is just
        // `dir/subdir`. Otherwise upstream's `implied_filter_list` check
        // (flist.c:1026) rejects `from/dir/subdir` as "unrequested".
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "from/./dir/subdir", false, true);
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
        let split = split_files_from_entry(&base, "from/./", true, true);
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
        let split = split_files_from_entry(&base, &sanitized, true, true);
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
        let split = split_files_from_entry(&base, "from/./dir/subdir/subsubdir2", true, true);
        assert_eq!(split.base, PathBuf::from("/src/from"));
        assert_eq!(split.path, PathBuf::from("/src/from/dir/subdir/subsubdir2"));
        assert!(split.recurse);
    }

    #[test]
    fn split_files_from_entry_collapses_redundant_separator_slashes() {
        // `dir/././sub` should behave like `dir/./sub`: head is `dir`, rest is `sub`.
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "dir/././sub", false, true);
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

    #[test]
    fn split_files_from_entry_plain_name_is_not_implied_dot() {
        // upstream: flist.c:2368 - `implied_dot_dir` only trips on a leading
        // `./`; a plain relative name never emits the transfer-root `.`.
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "dir/file.txt", false, true);
        assert!(!split.implied_dot);
    }

    #[test]
    fn split_files_from_entry_leading_dot_anchor_sets_implied_dot() {
        // upstream: flist.c:2368 - `*fn == '.' && fn[1] == '/' && fn[2]`. A
        // leading `./foo` files-from line (no embedded `/./`) marks the entry
        // so `--relative` mode emits a single FLAG_IMPLIED_DIR root `.`.
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "./foo/bar", false, true);
        assert!(split.implied_dot);
    }

    #[test]
    fn split_files_from_entry_bare_dot_forms_are_not_implied_dot() {
        // A bare `.` or `./` (upstream `fn[2]` is NUL) never sets the flag.
        let base = PathBuf::from("/src");
        assert!(!split_files_from_entry(&base, ".", false, true).implied_dot);
        assert!(!split_files_from_entry(&base, "./", true, true).implied_dot);
    }

    #[test]
    fn split_files_from_entry_anchor_then_leading_dot_sets_implied_dot() {
        // upstream: flist.c:2359-2368 - after the `/./` split, `fn` is the
        // suffix; `dir/././sub` leaves `rest == "./sub"`, which still trips
        // `implied_dot_dir`.
        let base = PathBuf::from("/src");
        let split = split_files_from_entry(&base, "dir/././sub", false, true);
        assert!(split.implied_dot);
    }
}
