#![deny(unsafe_code)]
//! Server-side Generator role implementation.
//!
//! When the native server operates as a Generator (sender), it:
//! 1. Walks the local filesystem to build a file list
//! 2. Sends the file list to the client (receiver)
//! 3. Receives signatures from the client for existing files
//! 4. Generates and sends deltas for each file

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use filters::{FilterRule, FilterSet};
use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};
use protocol::flist::{FileEntry, FileListWriter};

use super::config::ServerConfig;
use super::handshake::HandshakeResult;

/// Context for the generator role during a transfer.
#[derive(Debug)]
pub struct GeneratorContext {
    /// Negotiated protocol version.
    protocol: ProtocolVersion,
    /// Server configuration.
    config: ServerConfig,
    /// List of files to send.
    file_list: Vec<FileEntry>,
    /// Filter rules received from client.
    filters: Option<FilterSet>,
}

impl GeneratorContext {
    /// Creates a new generator context from handshake result and config.
    pub fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        Self {
            protocol: handshake.protocol,
            config,
            file_list: Vec::new(),
            filters: None,
        }
    }

    /// Returns the negotiated protocol version.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns a reference to the server configuration.
    #[must_use]
    pub const fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Returns the generated file list.
    #[must_use]
    pub fn file_list(&self) -> &[FileEntry] {
        &self.file_list
    }

    /// Builds the file list from the specified paths.
    ///
    /// This walks the filesystem starting from each path in the arguments
    /// and builds a sorted file list for transmission.
    pub fn build_file_list(&mut self, base_paths: &[PathBuf]) -> io::Result<usize> {
        self.file_list.clear();

        for base_path in base_paths {
            self.walk_path(base_path, base_path)?;
        }

        // Sort file list lexicographically (rsync requirement)
        self.file_list.sort_by(|a, b| a.name().cmp(b.name()));

        Ok(self.file_list.len())
    }

    /// Recursively walks a path and adds entries to the file list.
    fn walk_path(&mut self, base: &Path, path: &Path) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;

        // Calculate relative path
        let relative = path.strip_prefix(base).unwrap_or(path).to_path_buf();

        // Skip the base path itself if it's a directory
        if relative.as_os_str().is_empty() && metadata.is_dir() {
            // Walk children of the base directory
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                self.walk_path(base, &entry.path())?;
            }
            return Ok(());
        }

        // Check filters if present
        if let Some(ref filters) = self.filters {
            let is_dir = metadata.is_dir();
            if !filters.allows(&relative, is_dir) {
                // Path is excluded by filters, skip it
                eprintln!(
                    "[generator] Excluding {} ({})",
                    relative.display(),
                    if is_dir { "dir" } else { "file" }
                );
                return Ok(());
            }
        }

        // Create file entry based on type
        let entry = self.create_entry(path, &relative, &metadata)?;
        self.file_list.push(entry);

        // Recurse into directories if recursive mode is enabled
        if metadata.is_dir() && self.config.flags.recursive {
            for dir_entry in std::fs::read_dir(path)? {
                let dir_entry = dir_entry?;
                self.walk_path(base, &dir_entry.path())?;
            }
        }

        Ok(())
    }

    /// Creates a file entry from path and metadata.
    ///
    /// The `full_path` is used for filesystem operations (e.g., reading symlink targets),
    /// while `relative_path` is stored in the entry for transmission to the receiver.
    fn create_entry(
        &self,
        full_path: &Path,
        relative_path: &Path,
        metadata: &std::fs::Metadata,
    ) -> io::Result<FileEntry> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let file_type = metadata.file_type();

        let mut entry = if file_type.is_file() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = if metadata.permissions().readonly() {
                0o444
            } else {
                0o644
            };

            FileEntry::new_file(relative_path.to_path_buf(), metadata.len(), mode)
        } else if file_type.is_dir() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = 0o755;

            FileEntry::new_directory(relative_path.to_path_buf(), mode)
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(full_path).unwrap_or_else(|_| PathBuf::from(""));

            FileEntry::new_symlink(relative_path.to_path_buf(), target)
        } else {
            // Other file types (devices, etc.)
            FileEntry::new_file(relative_path.to_path_buf(), 0, 0o644)
        };

        // Set modification time
        #[cfg(unix)]
        {
            entry.set_mtime(metadata.mtime(), metadata.mtime_nsec() as u32);
        }
        #[cfg(not(unix))]
        {
            if let Ok(mtime) = metadata.modified() {
                if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_mtime(duration.as_secs() as i64, duration.subsec_nanos());
                }
            }
        }

        // Set ownership if preserving
        #[cfg(unix)]
        if self.config.flags.owner {
            entry.set_uid(metadata.uid());
        }
        #[cfg(unix)]
        if self.config.flags.group {
            entry.set_gid(metadata.gid());
        }

        Ok(entry)
    }

    /// Sends the file list to the receiver.
    pub fn send_file_list<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<usize> {
        // Capture output to a buffer so we can hex dump it
        let mut buffer = Vec::new();
        let mut flist_writer = FileListWriter::new(self.protocol);

        for entry in &self.file_list {
            flist_writer.write_entry(&mut buffer, entry)?;
        }

        flist_writer.write_end(&mut buffer)?;

        // Hex dump the file list data to both stderr and file
        let hex_len = buffer.len().min(256);
        eprintln!(
            "[generator] File list data ({} total bytes, showing first {}): {:02x?}",
            buffer.len(),
            hex_len,
            &buffer[..hex_len]
        );

        // Also write to file for easier analysis
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/rsync-filelist-debug.log")
        {
            use std::io::Write;
            let _ = writeln!(f, "=== File list ({} bytes) ===", buffer.len());
            let _ = writeln!(f, "{:02x?}", &buffer);
        }

        // Write to actual output
        writer.write_all(&buffer)?;
        writer.flush()?;

        Ok(self.file_list.len())
    }

    /// Runs the generator role to completion.
    ///
    /// This orchestrates the full send operation:
    /// 1. Build file list from paths
    /// 2. Send file list
    /// 3. For each file: receive signature, generate delta, send delta
    pub fn run<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        paths: &[PathBuf],
    ) -> io::Result<GeneratorStats> {
        // Read filter list from client (mirrors upstream recv_filter_list at main.c:1256)
        eprintln!("[generator] Reading filter list...");
        let wire_rules = read_filter_list(reader, self.protocol)?;
        eprintln!("[generator] Received {} filter rules", wire_rules.len());

        // Convert wire format to FilterSet
        if !wire_rules.is_empty() {
            let filter_set = self.parse_received_filters(&wire_rules)?;
            self.filters = Some(filter_set);
            eprintln!("[generator] Filter set initialized");
        } else {
            eprintln!("[generator] No filters received (empty list)");
        }

        // Build file list
        self.build_file_list(paths)?;
        eprintln!(
            "[generator] Built file list with {} entries",
            self.file_list.len()
        );

        // Send file list
        eprintln!("[generator] Sending file list...");
        let file_count = self.send_file_list(writer)?;
        eprintln!("[generator] File list sent ({file_count} files)");

        // Wait for client to send NDX_DONE (indicates file list received)
        // Mirrors upstream sender.c:read_ndx_and_attrs() flow
        // For protocol >= 30, NDX_DONE is encoded as single byte 0x00
        let mut ndx_byte = [0u8; 1];
        reader.read_exact(&mut ndx_byte)?;

        if ndx_byte[0] != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected NDX_DONE (0x00), got 0x{:02x}", ndx_byte[0]),
            ));
        }

        // Send NDX_DONE back to signal phase completion
        // Mirrors upstream sender.c:256 (write_ndx(f_out, NDX_DONE))
        writer.write_all(&[0])?;
        writer.flush()?;

        // For now, just report what we sent
        // Delta generation and sending will be implemented next
        Ok(GeneratorStats {
            files_listed: file_count,
            files_transferred: 0,
            bytes_sent: 0,
        })
    }

    /// Converts wire format rules to FilterSet.
    ///
    /// Maps the wire protocol representation to the filters crate's `FilterSet`
    /// for use during file walking.
    fn parse_received_filters(&self, wire_rules: &[FilterRuleWireFormat]) -> io::Result<FilterSet> {
        let mut rules = Vec::new();

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
                    // Merge rules not yet supported in server mode
                    // Skip for now; will be implemented in future phases
                    eprintln!(
                        "[generator] Skipping unsupported merge rule: {:?}",
                        wire_rule.rule_type
                    );
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

/// Statistics from a generator transfer operation.
#[derive(Debug, Clone, Default)]
pub struct GeneratorStats {
    /// Number of files in the sent file list.
    pub files_listed: usize,
    /// Number of files actually transferred.
    pub files_transferred: usize,
    /// Total bytes sent.
    pub bytes_sent: u64,
}

#[cfg(test)]
mod tests {
    use super::super::flags::ParsedServerFlags;
    use super::super::role::ServerRole;
    use super::*;
    use std::ffi::OsString;

    fn test_config() -> ServerConfig {
        ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_string(),
            flags: ParsedServerFlags::default(),
            args: vec![OsString::from(".")],
        }
    }

    fn test_handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
        }
    }

    #[test]
    fn generator_context_creation() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        assert_eq!(ctx.protocol().as_u8(), 32);
        assert!(ctx.file_list().is_empty());
    }

    #[test]
    fn send_empty_file_list() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        let count = ctx.send_file_list(&mut output).unwrap();

        assert_eq!(count, 0);
        // Should just have the end marker
        assert_eq!(output, vec![0u8]);
    }

    #[test]
    fn send_single_file_entry() {
        let handshake = test_handshake();
        let config = test_config();
        let mut ctx = GeneratorContext::new(&handshake, config);

        // Manually add an entry
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        ctx.file_list.push(entry);

        let mut output = Vec::new();
        let count = ctx.send_file_list(&mut output).unwrap();

        assert_eq!(count, 1);
        // Should have entry data plus end marker
        assert!(!output.is_empty());
        assert_eq!(*output.last().unwrap(), 0u8); // End marker
    }

    #[test]
    fn build_and_send_round_trip() {
        use super::super::receiver::ReceiverContext;
        use std::io::Cursor;

        let handshake = test_handshake();
        let mut gen_config = test_config();
        gen_config.role = ServerRole::Generator;
        let mut generator = GeneratorContext::new(&handshake, gen_config);

        // Add some entries manually (simulating a walk)
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_mtime(1700000000, 0);
        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        entry2.set_mtime(1700000000, 0);
        generator.file_list.push(entry1);
        generator.file_list.push(entry2);

        // Send file list
        let mut wire_data = Vec::new();
        generator.send_file_list(&mut wire_data).unwrap();

        // Receive file list
        let recv_config = test_config();
        let mut receiver = ReceiverContext::new(&handshake, recv_config);
        let mut cursor = Cursor::new(&wire_data[..]);
        let count = receiver.receive_file_list(&mut cursor).unwrap();

        assert_eq!(count, 2);
        assert_eq!(receiver.file_list()[0].name(), "file1.txt");
        assert_eq!(receiver.file_list()[1].name(), "file2.txt");
    }

    #[test]
    fn parse_received_filters_empty() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        // Empty filter list
        let wire_rules = vec![];
        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());

        let filter_set = result.unwrap();
        assert!(filter_set.is_empty());
    }

    #[test]
    fn parse_received_filters_single_exclude() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        use protocol::filters::FilterRuleWireFormat;

        let wire_rules = vec![FilterRuleWireFormat::exclude("*.log".to_string())];
        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());

        let filter_set = result.unwrap();
        assert!(!filter_set.is_empty());
    }

    #[test]
    fn parse_received_filters_multiple_rules() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        use protocol::filters::FilterRuleWireFormat;

        let wire_rules = vec![
            FilterRuleWireFormat::exclude("*.log".to_string()),
            FilterRuleWireFormat::include("*.txt".to_string()),
            FilterRuleWireFormat::exclude("temp/".to_string()).with_directory_only(true),
        ];

        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());

        let filter_set = result.unwrap();
        assert!(!filter_set.is_empty());
    }

    #[test]
    fn parse_received_filters_with_modifiers() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        use protocol::filters::FilterRuleWireFormat;

        let wire_rules = vec![
            FilterRuleWireFormat::exclude("*.tmp".to_string())
                .with_sides(true, false)
                .with_perishable(true),
            FilterRuleWireFormat::include("/important".to_string()).with_anchored(true),
        ];

        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_received_filters_clear_rule() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        use protocol::filters::{FilterRuleWireFormat, RuleType};

        let wire_rules = vec![
            FilterRuleWireFormat::exclude("*.log".to_string()),
            FilterRuleWireFormat {
                rule_type: RuleType::Clear,
                pattern: String::new(),
                anchored: false,
                directory_only: false,
                no_inherit: false,
                cvs_exclude: false,
                word_split: false,
                exclude_from_merge: false,
                xattr_only: false,
                sender_side: true,
                receiver_side: true,
                perishable: false,
                negate: false,
            },
            FilterRuleWireFormat::include("*.txt".to_string()),
        ];

        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());

        let filter_set = result.unwrap();
        // Clear rule should have removed previous rules
        assert!(!filter_set.is_empty()); // Only the include rule remains
    }

    #[test]
    fn filter_application_excludes_files() {
        use protocol::filters::FilterRuleWireFormat;
        use tempfile::TempDir;

        // Create temporary test directory
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test files
        std::fs::write(base_path.join("include.txt"), b"included").unwrap();
        std::fs::write(base_path.join("exclude.log"), b"excluded").unwrap();
        std::fs::write(base_path.join("another.txt"), b"also included").unwrap();

        // Set up generator with filter that excludes *.log
        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(base_path)];
        config.flags.recursive = false; // Don't recurse for this test

        let mut ctx = GeneratorContext::new(&handshake, config);

        // Parse and set filters
        let wire_rules = vec![FilterRuleWireFormat::exclude("*.log".to_string())];
        let filter_set = ctx.parse_received_filters(&wire_rules).unwrap();
        ctx.filters = Some(filter_set);

        // Build file list
        let paths = vec![base_path.to_path_buf()];
        let count = ctx.build_file_list(&paths).unwrap();

        // Should only have 2 files (the .txt files), not the .log file
        assert_eq!(count, 2);
        let file_list = ctx.file_list();
        assert_eq!(file_list.len(), 2);

        // Verify the .log file is not in the list
        for entry in file_list {
            assert!(!entry.name().contains(".log"));
        }
    }

    #[test]
    fn filter_application_includes_only_matching() {
        use protocol::filters::FilterRuleWireFormat;
        use tempfile::TempDir;

        // Create temporary test directory
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test files
        std::fs::write(base_path.join("data.txt"), b"text").unwrap();
        std::fs::write(base_path.join("script.sh"), b"shell").unwrap();
        std::fs::write(base_path.join("readme.md"), b"markdown").unwrap();

        // Set up generator with filters: exclude all, then include only *.txt
        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(base_path)];
        config.flags.recursive = false;

        let mut ctx = GeneratorContext::new(&handshake, config);

        // Parse and set filters: exclude *, include *.txt
        let wire_rules = vec![
            FilterRuleWireFormat::exclude("*".to_string()),
            FilterRuleWireFormat::include("*.txt".to_string()),
        ];
        let filter_set = ctx.parse_received_filters(&wire_rules).unwrap();
        ctx.filters = Some(filter_set);

        // Build file list
        let paths = vec![base_path.to_path_buf()];
        let count = ctx.build_file_list(&paths).unwrap();

        // Should only have 1 file (data.txt)
        assert_eq!(count, 1);
        let file_list = ctx.file_list();
        assert_eq!(file_list.len(), 1);
        assert_eq!(file_list[0].name(), "data.txt");
    }

    #[test]
    fn filter_application_with_directories() {
        use protocol::filters::FilterRuleWireFormat;
        use tempfile::TempDir;

        // Create temporary test directory structure
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        std::fs::create_dir(base_path.join("include_dir")).unwrap();
        std::fs::write(base_path.join("include_dir/file.txt"), b"data").unwrap();

        std::fs::create_dir(base_path.join("exclude_dir")).unwrap();
        std::fs::write(base_path.join("exclude_dir/file.txt"), b"data").unwrap();

        // Set up generator with filter that excludes exclude_dir/
        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(base_path)];
        config.flags.recursive = true;

        let mut ctx = GeneratorContext::new(&handshake, config);

        // Parse and set filters
        let wire_rules = vec![
            FilterRuleWireFormat::exclude("exclude_dir/".to_string()).with_directory_only(true),
        ];
        let filter_set = ctx.parse_received_filters(&wire_rules).unwrap();
        ctx.filters = Some(filter_set);

        // Build file list
        let paths = vec![base_path.to_path_buf()];
        let count = ctx.build_file_list(&paths).unwrap();

        // Should have include_dir and its file, but not exclude_dir
        assert!(count >= 2); // At least the directory and one file
        let file_list = ctx.file_list();

        // Verify exclude_dir is not in the list
        for entry in file_list {
            let name = entry.name();
            assert!(
                !name.contains("exclude_dir"),
                "Found excluded dir: {name}"
            );
        }
    }

    #[test]
    fn filter_application_no_filters_includes_all() {
        use tempfile::TempDir;

        // Create temporary test directory
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test files
        std::fs::write(base_path.join("file1.txt"), b"data1").unwrap();
        std::fs::write(base_path.join("file2.log"), b"data2").unwrap();
        std::fs::write(base_path.join("file3.md"), b"data3").unwrap();

        // Set up generator with NO filters
        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(base_path)];
        config.flags.recursive = false;

        let mut ctx = GeneratorContext::new(&handshake, config);
        // No filters set (ctx.filters remains None)

        // Build file list
        let paths = vec![base_path.to_path_buf()];
        let count = ctx.build_file_list(&paths).unwrap();

        // Should have all 3 files when no filters are present
        assert_eq!(count, 3);
        assert_eq!(ctx.file_list().len(), 3);
    }
}
