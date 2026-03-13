//! Filter rule handling and files-from path resolution.

use std::io::{self, Read};
use std::path::PathBuf;

pub use ::filters::FilterSet;
use logging::info_log;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};

use ::filters::FilterRule;
use super::GeneratorContext;

impl GeneratorContext {
    /// Receives filter list from client in server mode.
    ///
    /// In server mode, we receive filter rules from the client before building
    /// the file list. In client mode, we already sent filters in mod.rs.
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
            // upstream: flist.c:1332 — is_excluded() applied during make_file()
            if !self.config.connection.filter_rules.is_empty() {
                let filter_set =
                    self.parse_received_filters(&self.config.connection.filter_rules.clone())?;
                self.filters = Some(filter_set);
            }
            return Ok(());
        }

        // Server mode: read filter list from client (MULTIPLEXED for protocol >= 30)
        let wire_rules = read_filter_list(reader, self.protocol)?;

        // upstream: clientserver.c:rsync_module() — daemon_filter_list is applied
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

        // Convert wire format to FilterSet
        if !combined.is_empty() {
            let filter_set = self.parse_received_filters(&combined)?;
            self.filters = Some(filter_set);
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
    /// base source directory). Returns an empty `Vec` when no `--files-from` is
    /// configured, signaling the caller to use the original positional paths.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2262` - `read_line(filesfrom_fd, ...)` reads one name at a time
    /// - `main.c:681-685` - `filesfrom_fd` set to `STDIN_FILENO` for `--files-from=-`
    /// - `io.c:start_filesfrom_forwarding()` - client forwards local file over socket
    pub(super) fn resolve_files_from_paths<R: Read>(
        &self,
        original_paths: &[PathBuf],
        reader: &mut R,
    ) -> io::Result<Vec<PathBuf>> {
        let files_from_path = match &self.config.file_selection.files_from_path {
            Some(path) => path.clone(),
            None => return Ok(Vec::new()),
        };

        // Determine base directory: use the first positional arg (source dir).
        // upstream: flist.c:2220 — dir = argv[0] when files_from is active.
        let base_dir = original_paths
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));

        let filenames = if files_from_path == "-" {
            // Read from protocol stream (stdin). The client forwards the file
            // list as NUL-separated entries with a double-NUL terminator.
            // upstream: main.c:681 — filesfrom_fd = STDIN_FILENO
            protocol::read_files_from_stream(reader)?
        } else {
            // Read from a local file on the server.
            // upstream: main.c:675-679 — open(files_from, O_RDONLY)
            let from0 = self.config.file_selection.from0;
            read_files_from_local_path(&files_from_path, from0)?
        };

        let mut resolved = Vec::with_capacity(filenames.len());
        for name in &filenames {
            if name.is_empty() {
                continue;
            }
            let path = base_dir.join(name);
            resolved.push(path);
        }

        info_log!(
            Flist,
            1,
            "read {} filenames from --files-from source",
            resolved.len()
        );

        Ok(resolved)
    }

    /// Converts wire format rules to FilterSet.
    ///
    /// Maps the wire protocol representation to the filters crate's `FilterSet`
    /// for use during file walking.
    pub(super) fn parse_received_filters(
        &self,
        wire_rules: &[FilterRuleWireFormat],
    ) -> io::Result<FilterSet> {
        let mut rules = Vec::with_capacity(wire_rules.len());

        for wire_rule in wire_rules {
            // Convert wire RuleType to FilterRule
            let mut rule = match wire_rule.rule_type {
                RuleType::Include => FilterRule::include(&wire_rule.pattern),
                RuleType::Exclude => FilterRule::exclude(&wire_rule.pattern),
                RuleType::Protect => FilterRule::protect(&wire_rule.pattern),
                RuleType::Risk => FilterRule::risk(&wire_rule.pattern),
                RuleType::Clear => {
                    // Clear rule removes all previous rules
                    rules.push(
                        FilterRule::clear()
                            .with_sides(wire_rule.sender_side, wire_rule.receiver_side),
                    );
                    continue;
                }
                RuleType::Merge | RuleType::DirMerge => {
                    // Merge rules require per-directory filter file loading during file walking.
                    // Implementation requires:
                    // 1. Store merge rule specs (filename, options like inherit/exclude_self)
                    // 2. During build_file_list(), check each directory for the merge file
                    // 3. Parse merge file contents using engine::local_copy::dir_merge parsing
                    // 4. Inject parsed rules into the active FilterSet for that subtree
                    // 5. Pop rules when leaving directories (if no_inherit is set)
                    //
                    // See crates/engine/src/local_copy/dir_merge/ for the local copy implementation
                    // that can be adapted for server mode. The challenge is that FilterSet is
                    // currently immutable after construction.
                    //
                    // For now, clients can pre-expand merge rules before transmission, or use
                    // local copy mode which fully supports merge rules.
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

            if wire_rule.anchored {
                rule = rule.anchor_to_root();
            }

            // Note: directory_only, no_inherit, cvs_exclude, word_split, exclude_from_merge
            // are pattern modifiers handled by the filters crate during compilation
            // We store them in the pattern itself as upstream does

            rules.push(rule);
        }

        FilterSet::from_rules(rules)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("filter error: {e}")))
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
/// - `flist.c:2262` - `read_line(filesfrom_fd, ...)` reads lines
pub(super) fn read_files_from_local_path(path: &str, from0: bool) -> io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);

    if from0 {
        // NUL-delimited: use the wire format reader which handles NUL separators.
        protocol::read_files_from_stream(&mut reader)
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
            // upstream: flist.c:2266 — comments with '#' or ';' prefix
            // only when not using --from0
            if trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            filenames.push(trimmed.to_owned());
        }
        Ok(filenames)
    }
}
