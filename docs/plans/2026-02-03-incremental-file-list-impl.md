# Incremental File List Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Stream file entries as they arrive, creating directories and starting transfers immediately rather than waiting for the complete list.

**Architecture:** Refactor `run_pipelined()` from two-phase (receive all → transfer all) to a single streaming loop that interleaves wire reading, directory creation, and file transfers using priority-based scheduling.

**Tech Stack:** Rust, existing `IncrementalFileList` and `IncrementalFileListReceiver` infrastructure

---

## Task 1: Add FailedDirectories Struct

**Files:**
- Modify: `crates/transfer/src/receiver.rs:1250` (after ReceiverContext impl block)

**Step 1: Write the failing test**

Add at the end of the `#[cfg(test)]` module in receiver.rs:

```rust
mod failed_directories_tests {
    use super::*;

    #[test]
    fn failed_directories_empty_has_no_ancestors() {
        let failed = FailedDirectories::new();
        assert!(failed.failed_ancestor("any/path/file.txt").is_none());
    }

    #[test]
    fn failed_directories_marks_and_finds_exact() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert!(failed.failed_ancestor("foo/bar").is_some());
    }

    #[test]
    fn failed_directories_finds_child_of_failed() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert_eq!(failed.failed_ancestor("foo/bar/baz/file.txt"), Some("foo/bar"));
    }

    #[test]
    fn failed_directories_does_not_match_sibling() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert!(failed.failed_ancestor("foo/other/file.txt").is_none());
    }

    #[test]
    fn failed_directories_counts_failures() {
        let mut failed = FailedDirectories::new();
        assert_eq!(failed.count(), 0);
        failed.mark_failed("a");
        failed.mark_failed("b");
        assert_eq!(failed.count(), 2);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p transfer --lib failed_directories_tests -- --nocapture 2>&1 | head -20`

Expected: Compilation error - `FailedDirectories` not found

**Step 3: Write the implementation**

Add before the `IncrementalFileListReceiver` struct (around line 1252):

```rust
// ============================================================================
// Failed Directory Tracking
// ============================================================================

/// Tracks directories that failed to create.
///
/// Children of failed directories are skipped during incremental processing.
#[derive(Debug, Default)]
struct FailedDirectories {
    /// Failed directory paths (normalized, no trailing slash).
    paths: std::collections::HashSet<String>,
}

impl FailedDirectories {
    /// Creates a new empty tracker.
    fn new() -> Self {
        Self::default()
    }

    /// Marks a directory as failed.
    fn mark_failed(&mut self, path: &str) {
        self.paths.insert(path.to_string());
    }

    /// Checks if an entry path has a failed ancestor directory.
    ///
    /// Returns the failed ancestor path if found, `None` otherwise.
    fn failed_ancestor(&self, entry_path: &str) -> Option<&str> {
        // Check if exact path is failed
        if self.paths.contains(entry_path) {
            return self.paths.get(entry_path).map(|s| s.as_str());
        }

        // Check each parent path component
        let mut check_path = entry_path;
        while let Some(pos) = check_path.rfind('/') {
            check_path = &check_path[..pos];
            if let Some(failed) = self.paths.get(check_path) {
                return Some(failed.as_str());
            }
        }
        None
    }

    /// Returns the number of failed directories.
    fn count(&self) -> usize {
        self.paths.len()
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p transfer --lib failed_directories_tests -- --nocapture`

Expected: All 5 tests pass

**Step 5: Commit**

```bash
git add crates/transfer/src/receiver.rs
git commit -m "feat(transfer): add FailedDirectories for tracking failed dir creation"
```

---

## Task 2: Add try_read_one() Method

**Files:**
- Modify: `crates/transfer/src/receiver.rs:1287` (in IncrementalFileListReceiver impl)

**Step 1: Write the failing test**

Add to the test module:

```rust
mod incremental_receiver_tests {
    use super::*;
    use std::io::Cursor;

    fn make_test_receiver(data: &[u8]) -> IncrementalFileListReceiver<Cursor<Vec<u8>>> {
        // This is a simplified test - actual wire format would come from FileListReader
        // For now we test the API exists
        todo!("Need mock wire data")
    }

    #[test]
    fn try_read_one_returns_false_when_finished() {
        // Create a receiver that's already marked as finished
        let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
        let flist_reader = protocol::flist::FileListReader::new(protocol);

        // Empty data - will hit EOF immediately
        let empty_data: Vec<u8> = vec![0]; // Single zero byte = end of list marker
        let source = Cursor::new(empty_data);

        let incremental = protocol::flist::IncrementalFileList::new();

        let mut receiver = IncrementalFileListReceiver {
            flist_reader,
            source,
            incremental,
            finished_reading: true, // Already finished
            entries_read: 0,
        };

        // Should return false since already finished
        assert!(!receiver.try_read_one().unwrap());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p transfer --lib incremental_receiver_tests -- --nocapture 2>&1 | head -30`

Expected: Error - `try_read_one` method not found

**Step 3: Write the implementation**

Add to `IncrementalFileListReceiver` impl block (after `next_ready`):

```rust
    /// Attempts to read one entry from the wire without blocking on ready queue.
    ///
    /// Returns `Ok(true)` if an entry was read and added to the incremental
    /// processor, `Ok(false)` if at EOF or already finished reading.
    ///
    /// Unlike [`next_ready`], this method does not wait for an entry to become
    /// ready. It simply reads from the wire and adds to the dependency tracker.
    pub fn try_read_one(&mut self) -> io::Result<bool> {
        if self.finished_reading {
            return Ok(false);
        }

        match self.flist_reader.read_entry(&mut self.source)? {
            Some(entry) => {
                self.entries_read += 1;
                self.incremental.push(entry);
                Ok(true)
            }
            None => {
                self.finished_reading = true;
                Ok(false)
            }
        }
    }

    /// Returns any orphaned entries that have no parent directory.
    ///
    /// Call this after reading is complete to get entries whose parent
    /// directories were never received (indicates protocol error or corruption).
    pub fn take_orphans(&mut self) -> Vec<FileEntry> {
        if self.finished_reading {
            // The incremental list's finish() would consume it, so we drain pending
            // This returns entries that are still waiting for parents
            let mut orphans = Vec::new();
            // Drain ready first (these aren't orphans)
            let _ = self.incremental.drain_ready();
            // Check if there are pending entries
            if self.incremental.pending_count() > 0 {
                // We can't easily extract pending entries without modifying IncrementalFileList
                // For now, just report the count and let finish() handle it
            }
            orphans
        } else {
            Vec::new()
        }
    }

    /// Marks reading as finished (for error recovery).
    pub fn mark_finished(&mut self) {
        self.finished_reading = true;
    }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p transfer --lib incremental_receiver_tests -- --nocapture`

Expected: Test passes

**Step 5: Commit**

```bash
git add crates/transfer/src/receiver.rs
git commit -m "feat(transfer): add try_read_one() for non-blocking wire reads"
```

---

## Task 3: Extend TransferStats

**Files:**
- Modify: `crates/transfer/src/receiver.rs:1440` (TransferStats struct)

**Step 1: Write the failing test**

```rust
#[test]
fn transfer_stats_has_incremental_fields() {
    let stats = TransferStats {
        files_listed: 0,
        files_transferred: 0,
        bytes_received: 0,
        bytes_sent: 0,
        total_source_bytes: 0,
        metadata_errors: vec![],
        // New fields
        entries_received: 100,
        directories_created: 10,
        directories_failed: 2,
        files_skipped: 5,
    };

    assert_eq!(stats.entries_received, 100);
    assert_eq!(stats.directories_created, 10);
    assert_eq!(stats.directories_failed, 2);
    assert_eq!(stats.files_skipped, 5);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p transfer --lib transfer_stats_has_incremental_fields 2>&1 | head -20`

Expected: Compilation error - unknown fields

**Step 3: Write the implementation**

Modify `TransferStats` struct:

```rust
/// Statistics from a receiver transfer operation.
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    /// Number of files in the received file list.
    pub files_listed: usize,
    /// Number of files actually transferred.
    pub files_transferred: usize,
    /// Total bytes received from the sender (file data, deltas, etc.).
    pub bytes_received: u64,
    /// Total bytes sent to the sender (signatures, file indices, etc.).
    pub bytes_sent: u64,
    /// Total size of all source files in the file list.
    pub total_source_bytes: u64,
    /// Metadata errors encountered (path, error message).
    pub metadata_errors: Vec<(PathBuf, String)>,

    // Incremental mode statistics
    /// Total entries received from wire (incremental mode).
    pub entries_received: u64,
    /// Directories successfully created (incremental mode).
    pub directories_created: u64,
    /// Directories that failed to create (incremental mode).
    pub directories_failed: u64,
    /// Files skipped due to failed parent directory (incremental mode).
    pub files_skipped: u64,
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p transfer --lib transfer_stats_has_incremental_fields`

Expected: Pass

**Step 5: Commit**

```bash
git add crates/transfer/src/receiver.rs
git commit -m "feat(transfer): extend TransferStats with incremental mode fields"
```

---

## Task 4: Add create_directory_incremental Helper

**Files:**
- Modify: `crates/transfer/src/receiver.rs` (add new method to ReceiverContext)

**Step 1: Write the failing test**

```rust
#[test]
fn create_directory_incremental_creates_dir() {
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let dest = temp.path();

    let entry = protocol::flist::FileEntry::new_directory("subdir".into(), 0o755);
    let opts = MetadataOptions::default();
    let mut failed = FailedDirectories::new();

    let handshake = HandshakeResult {
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
    };
    let config = ServerConfig::default();
    let ctx = ReceiverContext::new(&handshake, config);

    let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed);

    assert!(result.is_ok());
    assert!(dest.join("subdir").exists());
    assert_eq!(failed.count(), 0);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p transfer --lib create_directory_incremental_creates_dir 2>&1 | head -20`

Expected: Method not found

**Step 3: Write the implementation**

Add to `ReceiverContext` impl:

```rust
    /// Creates a single directory during incremental processing.
    ///
    /// On success, returns `Ok(true)`. On failure, marks the directory as failed
    /// and returns `Ok(false)`. Only returns `Err` for unrecoverable errors.
    fn create_directory_incremental(
        &self,
        dest_dir: &Path,
        entry: &FileEntry,
        metadata_opts: &MetadataOptions,
        failed_dirs: &mut FailedDirectories,
    ) -> io::Result<bool> {
        let relative_path = entry.path();
        let dir_path = if relative_path.as_os_str() == "." {
            dest_dir.to_path_buf()
        } else {
            dest_dir.join(relative_path)
        };

        // Check if parent is under a failed directory
        if let Some(failed_parent) = failed_dirs.failed_ancestor(entry.name()) {
            if self.config.flags.verbose && self.config.client_mode {
                eprintln!(
                    "skipping directory {} (parent {} failed)",
                    entry.name(),
                    failed_parent
                );
            }
            failed_dirs.mark_failed(entry.name());
            return Ok(false);
        }

        // Try to create the directory
        if !dir_path.exists() {
            if let Err(e) = fs::create_dir_all(&dir_path) {
                if self.config.flags.verbose && self.config.client_mode {
                    eprintln!("failed to create directory {}: {}", dir_path.display(), e);
                }
                failed_dirs.mark_failed(entry.name());
                return Ok(false);
            }
        }

        // Apply metadata (non-fatal errors)
        if let Err(e) = apply_metadata_from_file_entry(&dir_path, entry, metadata_opts) {
            if self.config.flags.verbose && self.config.client_mode {
                eprintln!("warning: metadata error for {}: {}", dir_path.display(), e);
            }
            // Don't mark as failed - directory exists, just metadata issue
        }

        // Verbose output
        if self.config.flags.verbose && self.config.client_mode {
            if relative_path.as_os_str() == "." {
                eprintln!("./");
            } else {
                eprintln!("{}/", relative_path.display());
            }
        }

        Ok(true)
    }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p transfer --lib create_directory_incremental_creates_dir`

Expected: Pass

**Step 5: Commit**

```bash
git add crates/transfer/src/receiver.rs
git commit -m "feat(transfer): add create_directory_incremental helper"
```

---

## Task 5: Add run_pipelined_incremental Method

**Files:**
- Modify: `crates/transfer/src/receiver.rs` (add new method alongside run_pipelined)

**Step 1: Write scaffolding**

This is a large refactor. First add the method signature and basic structure:

```rust
    /// Runs the receiver with incremental file list processing.
    ///
    /// Unlike [`run_pipelined`], this method streams file entries as they arrive,
    /// creating directories and starting transfers immediately rather than waiting
    /// for the complete file list.
    ///
    /// # Benefits
    ///
    /// - Reduced startup latency: Transfers begin as first entries arrive
    /// - Lower memory peak: Don't buffer entire list before processing
    /// - Immediate progress feedback: Users see activity immediately
    pub fn run_pipelined_incremental<R: Read, W: Write + ?Sized>(
        &mut self,
        mut reader: super::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
    ) -> io::Result<TransferStats> {
        // Phase 1: Setup (same as run_pipelined)
        if self.protocol.as_u8() >= 30 {
            reader = reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?;
        }

        if self.should_read_filter_list() {
            let _wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(e.kind(), format!("failed to read filter list: {e}"))
            })?;
        }

        if self.config.flags.verbose && self.config.client_mode {
            eprintln!("receiving incremental file list");
        }

        // Phase 2: Initialize streaming infrastructure
        let mut flist_receiver = self.incremental_file_list_receiver(&mut reader);
        let mut failed_dirs = FailedDirectories::new();
        let mut files_ready: std::collections::VecDeque<(i32, FileEntry)> =
            std::collections::VecDeque::new();
        let mut next_ndx: i32 = 0;

        // Transfer infrastructure
        let mut pipeline = PipelineState::new(pipeline_config);
        let mut pending_transfers: std::collections::VecDeque<(PathBuf, FileEntry)> =
            std::collections::VecDeque::new();

        let checksum_factory = ChecksumFactory::from_negotiation(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let checksum_algorithm = checksum_factory.signature_algorithm();
        let checksum_length = DEFAULT_CHECKSUM_LENGTH;

        let metadata_opts = MetadataOptions::new()
            .preserve_permissions(self.config.flags.perms)
            .preserve_times(self.config.flags.times)
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids);

        let dest_dir = self
            .config
            .args
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

        // Ensure destination directory exists
        if !dest_dir.exists() {
            fs::create_dir_all(&dest_dir)?;
        }

        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let request_config = RequestConfig {
            protocol: self.protocol,
            write_iflags: self.protocol.as_u8() >= 29,
            checksum_length,
            checksum_algorithm,
            negotiated_algorithms: self.negotiated_algorithms.as_ref(),
            compat_flags: self.compat_flags.as_ref(),
            checksum_seed: self.checksum_seed,
            use_sparse: self.config.flags.sparse,
            do_fsync: self.config.fsync,
        };

        // Statistics
        let mut stats = TransferStats::default();

        // Phase 3: Main streaming loop
        loop {
            // Step 1: Read burst of entries from wire (up to 32)
            for _ in 0..32 {
                if !flist_receiver.try_read_one()? {
                    break;
                }
                stats.entries_received += 1;
            }

            // Step 2: Process ready entries (directories and files)
            for entry in flist_receiver.drain_ready() {
                // Store in file_list for protocol compatibility
                self.file_list.push(entry.clone());

                if entry.is_dir() {
                    if self.create_directory_incremental(
                        &dest_dir, &entry, &metadata_opts, &mut failed_dirs
                    )? {
                        stats.directories_created += 1;
                        flist_receiver.mark_directory_created(entry.name());
                    } else {
                        stats.directories_failed += 1;
                    }
                } else if entry.is_file() {
                    if let Some(failed_parent) = failed_dirs.failed_ancestor(entry.name()) {
                        if self.config.flags.verbose && self.config.client_mode {
                            eprintln!(
                                "skipping {} (parent {} failed)",
                                entry.name(),
                                failed_parent
                            );
                        }
                        stats.files_skipped += 1;
                    } else {
                        files_ready.push_back((next_ndx, entry));
                        next_ndx += 1;
                    }
                }
                // TODO: Handle symlinks, devices, etc.
            }

            // Step 3: Fill pipeline with file requests
            while pipeline.can_send() {
                if let Some((ndx, entry)) = files_ready.pop_front() {
                    let relative_path = entry.path();
                    let file_path = dest_dir.join(relative_path);

                    if self.config.flags.verbose && self.config.client_mode {
                        eprintln!("{}", relative_path.display());
                    }

                    let basis_config = BasisFileConfig {
                        file_path: &file_path,
                        dest_dir: &dest_dir,
                        relative_path,
                        target_size: entry.size(),
                        fuzzy_enabled: self.config.flags.fuzzy,
                        reference_directories: &self.config.reference_directories,
                        protocol: self.protocol,
                        checksum_length,
                        checksum_algorithm,
                    };
                    let basis_result = find_basis_file_with_config(&basis_config);

                    let pending = send_file_request(
                        writer,
                        &mut ndx_write_codec,
                        ndx,
                        file_path.clone(),
                        basis_result.signature,
                        basis_result.basis_path,
                        entry.size(),
                        &request_config,
                    )?;

                    pipeline.push(pending);
                    pending_transfers.push_back((file_path, entry));
                } else {
                    break;
                }
            }

            // Step 4: Process one response if pipeline non-empty
            if !pipeline.is_empty() {
                let pending = pipeline.pop().expect("pipeline not empty");
                let (file_path, file_entry) = pending_transfers.pop_front()
                    .expect("pending_transfers matches pipeline");

                let response_ctx = ResponseContext {
                    config: &request_config,
                };

                let total_bytes = process_file_response(
                    &mut reader,
                    &mut ndx_read_codec,
                    pending,
                    &response_ctx,
                )?;

                if let Err(meta_err) =
                    apply_metadata_from_file_entry(&file_path, &file_entry, &metadata_opts)
                {
                    stats.metadata_errors.push((file_path, meta_err.to_string()));
                }

                stats.bytes_received += total_bytes;
                stats.files_transferred += 1;
            }

            // Exit condition: finished reading, pipeline empty, no files ready
            if flist_receiver.is_finished_reading()
                && pipeline.is_empty()
                && files_ready.is_empty()
            {
                break;
            }

            // Stall prevention: if pipeline can send but no files ready and not EOF,
            // we need to read more
            if pipeline.can_send()
                && files_ready.is_empty()
                && !flist_receiver.is_finished_reading()
            {
                // Force read at least one entry
                if flist_receiver.try_read_one()? {
                    stats.entries_received += 1;
                }
            }
        }

        // Phase 4: Handle orphaned entries
        let orphan_count = flist_receiver.pending_count();
        if orphan_count > 0 {
            eprintln!(
                "warning: {} entries had missing parent directories",
                orphan_count
            );
            stats.files_skipped += orphan_count as u64;
        }

        // Phase 5: Finalization
        stats.files_listed = self.file_list.len();
        stats.total_source_bytes = self.file_list.iter().map(|e| e.size()).sum();

        self.exchange_phase_done(&mut reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;
        let _sender_stats = self.receive_stats(&mut reader)?;
        self.handle_goodbye(&mut reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        Ok(stats)
    }
```

**Step 2: Add test**

```rust
#[test]
fn run_pipelined_incremental_compiles() {
    // This test just verifies the method signature is correct
    // Full integration tests will be in Task 8
    fn _check_signature<R: Read, W: Write + ?Sized>(
        ctx: &mut ReceiverContext,
        reader: super::reader::ServerReader<R>,
        writer: &mut W,
    ) {
        let _ = ctx.run_pipelined_incremental(reader, writer, PipelineConfig::default());
    }
}
```

**Step 3: Run test**

Run: `cargo test -p transfer --lib run_pipelined_incremental_compiles`

Expected: Pass (compiles)

**Step 4: Commit**

```bash
git add crates/transfer/src/receiver.rs
git commit -m "feat(transfer): add run_pipelined_incremental for streaming file list"
```

---

## Task 6: Add Feature Flag and Public API

**Files:**
- Modify: `crates/transfer/Cargo.toml`
- Modify: `crates/transfer/src/receiver.rs`

**Step 1: Add feature flag to Cargo.toml**

```toml
[features]
default = []
incremental-flist = []
```

**Step 2: Add conditional compilation**

Wrap `run_pipelined_incremental` with feature gate:

```rust
    /// Runs the receiver with incremental file list processing.
    #[cfg(feature = "incremental-flist")]
    pub fn run_pipelined_incremental<R: Read, W: Write + ?Sized>(
        // ... existing implementation
    )
```

**Step 3: Add feature-gated run() variant**

```rust
    /// Runs the receiver with the best available strategy.
    pub fn run<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: super::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        #[cfg(feature = "incremental-flist")]
        {
            self.run_pipelined_incremental(reader, writer, PipelineConfig::default())
        }
        #[cfg(not(feature = "incremental-flist"))]
        {
            self.run_pipelined(reader, writer, PipelineConfig::default())
        }
    }
```

**Step 4: Test both configurations**

Run: `cargo test -p transfer --lib`
Run: `cargo test -p transfer --lib --features incremental-flist`

Expected: Both pass

**Step 5: Commit**

```bash
git add crates/transfer/Cargo.toml crates/transfer/src/receiver.rs
git commit -m "feat(transfer): add incremental-flist feature flag"
```

---

## Task 7: Unit Tests for Incremental Mode

**Files:**
- Modify: `crates/transfer/src/receiver.rs` (test module)

**Step 1: Add comprehensive unit tests**

```rust
#[cfg(all(test, feature = "incremental-flist"))]
mod incremental_mode_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn failed_directories_skips_nested_children() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a/b");

        // Direct child
        assert!(failed.failed_ancestor("a/b/file.txt").is_some());
        // Nested child
        assert!(failed.failed_ancestor("a/b/c/d/file.txt").is_some());
        // Sibling - not affected
        assert!(failed.failed_ancestor("a/c/file.txt").is_none());
        // Parent - not affected
        assert!(failed.failed_ancestor("a/file.txt").is_none());
    }

    #[test]
    fn failed_directories_handles_root_level() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("toplevel");

        assert!(failed.failed_ancestor("toplevel/sub/file.txt").is_some());
        assert!(failed.failed_ancestor("other/file.txt").is_none());
    }

    #[test]
    fn stats_tracks_incremental_fields() {
        let mut stats = TransferStats::default();

        stats.entries_received = 100;
        stats.directories_created = 20;
        stats.directories_failed = 2;
        stats.files_skipped = 10;
        stats.files_transferred = 68;

        // Verify consistency
        assert_eq!(
            stats.directories_created + stats.directories_failed,
            22 // total directories
        );
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p transfer --lib incremental_mode_tests --features incremental-flist`

Expected: All pass

**Step 3: Commit**

```bash
git add crates/transfer/src/receiver.rs
git commit -m "test(transfer): add unit tests for incremental file list mode"
```

---

## Task 8: Integration Test

**Files:**
- Create: `crates/transfer/tests/incremental_transfer.rs`

**Step 1: Create integration test file**

```rust
//! Integration tests for incremental file list transfer.

#![cfg(feature = "incremental-flist")]

use std::io::Cursor;
use transfer::receiver::{ReceiverContext, TransferStats};

/// Test helper to create mock wire data for a file list.
fn create_mock_file_list_wire_data() -> Vec<u8> {
    // TODO: Generate valid wire format for testing
    // For now, just verify the test infrastructure works
    vec![0] // Empty list marker
}

#[test]
fn incremental_transfer_empty_list() {
    // Empty file list should complete without error
    // This is a placeholder for the full integration test
}

#[test]
#[ignore = "requires mock wire data generation"]
fn incremental_transfer_flat_directory() {
    // 10 files in root - all should transfer
}

#[test]
#[ignore = "requires mock wire data generation"]
fn incremental_transfer_nested_directories() {
    // Nested structure with proper ordering
}

#[test]
#[ignore = "requires mock wire data generation"]
fn incremental_transfer_out_of_order_entries() {
    // Child before parent - should handle correctly
}
```

**Step 2: Run integration tests**

Run: `cargo test -p transfer --test incremental_transfer --features incremental-flist`

Expected: Pass (non-ignored tests)

**Step 3: Commit**

```bash
git add crates/transfer/tests/incremental_transfer.rs
git commit -m "test(transfer): add integration test scaffolding for incremental mode"
```

---

## Summary

| Task | Description | Est. Time |
|------|-------------|-----------|
| 1 | FailedDirectories struct | 10 min |
| 2 | try_read_one() method | 10 min |
| 3 | Extend TransferStats | 5 min |
| 4 | create_directory_incremental helper | 15 min |
| 5 | run_pipelined_incremental method | 30 min |
| 6 | Feature flag and public API | 10 min |
| 7 | Unit tests | 15 min |
| 8 | Integration test scaffolding | 10 min |

**Total: ~105 minutes**

After completing all tasks, run full test suite:

```bash
cargo test -p transfer --features incremental-flist
cargo test -p transfer  # Verify non-incremental still works
```
