# ServerConfig Decomposition Implementation Plan

> **For implementers:** Work through this plan task-by-task; complete each before moving to the next.

**Goal:** Break ServerConfig (27 fields) into focused sub-configs using the Parameter Object pattern, reducing construction boilerplate from 27 fields to ~5 grouped structs, each with `Default`.

**Architecture:** Extract cohesive field groups into dedicated sub-structs with `#[derive(Default)]`. ServerConfig becomes a composition of sub-configs plus core identity fields. All sub-structs implement `Default` so test construction sites use `..Default::default()` instead of enumerating every field.

**Tech Stack:** Rust structs, `#[derive(Debug, Clone, Eq, PartialEq, Default)]`

---

## Analysis: Field Access Patterns

| Field Group | Fields | Accessed By |
|-------------|--------|-------------|
| **Core identity** | `role`, `protocol`, `flag_string`, `flags`, `args` | Both receiver + generator (always needed) |
| **File selection** | `min_file_size`, `max_file_size`, `ignore_existing`, `existing_only`, `size_only`, `files_from_path`, `from0` | Receiver (`build_files_to_transfer`), generator (file list building) |
| **Write behavior** | `fsync`, `inplace`, `write_devices`, `io_uring_policy` | Receiver (transfer ops, file writing) |
| **Deletion** | `max_delete`, `ignore_errors` | Receiver (delete logic), generator (io_error marker) |
| **Connection context** | `client_mode`, `is_daemon_connection`, `filter_rules`, `iconv`, `compression_level` | Both (handshake, filter exchange, file list encoding) |
| **Protocol tuning** | `checksum_seed`, `checksum_choice`, `qsort`, `trust_sender`, `stop_at`, `reference_directories` | Various |

## Construction Sites (all must be updated)

1. `crates/transfer/src/config.rs:286` - `from_flag_string_and_args()`
2. `crates/transfer/src/receiver.rs:3644` - `test_config()`
3. `crates/transfer/src/receiver.rs:4546` - `config_with_flags()`
4. `crates/transfer/src/generator.rs:2588` - `test_config()`
5. `crates/transfer/src/generator.rs:3823` - `config_with_role_and_flags()`
6. `crates/transfer/src/tests/negotiated_algorithms.rs:32` - `test_config()`
7. `crates/transfer/src/tests/negotiated_algorithms.rs:66` - `test_config_with_compression_level()`

## Approach: Bottom-Up, One Sub-Config at a Time

Each task extracts one group, adds `Default`, updates all 7 construction sites, and verifies compilation. This is safe because each step is independently compilable and testable.

---

### Task 1: Add `Default` to `ServerConfig` (prerequisite)

**Files:**
- Modify: `crates/transfer/src/config.rs:19-262`

**Why first:** Adding `Default` immediately lets test sites use `..Default::default()`, reducing the blast radius of subsequent extractions.

**Step 1: Implement `Default` for `ServerConfig`**

Cannot use `#[derive(Default)]` because `protocol` defaults to `ProtocolVersion::NEWEST` (not the `Default` trait impl), and `flag_string`/`flags` have no meaningful defaults. Instead, implement manually:

```rust
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::NEWEST,
            flag_string: String::new(),
            flags: ParsedServerFlags::default(),
            args: Vec::new(),
            compression_level: None,
            client_mode: false,
            filter_rules: Vec::new(),
            reference_directories: Vec::new(),
            iconv: None,
            ignore_errors: false,
            fsync: false,
            checksum_seed: None,
            is_daemon_connection: false,
            checksum_choice: None,
            write_devices: false,
            trust_sender: false,
            stop_at: None,
            qsort: false,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
            min_file_size: None,
            max_file_size: None,
            files_from_path: None,
            from0: false,
            inplace: false,
            size_only: false,
            ignore_existing: false,
            existing_only: false,
            max_delete: None,
        }
    }
}
```

**Step 2: Simplify all 7 test construction sites**

Replace full field enumeration with struct update syntax. Example for `receiver.rs:test_config()`:

```rust
fn test_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        ..Default::default()
    }
}
```

Same pattern for all 7 sites - only specify fields that differ from defaults.

**Step 3: Verify compilation**

Run: `cargo check -p transfer --all-features`
Expected: clean compilation

**Step 4: Run tests**

Run: `cargo nextest run -p transfer --all-features -E 'test(config) | test(negotiated) | test(receiver_context) | test(generator_context)'`
Expected: all pass

**Step 5: Commit**

```bash
git add crates/transfer/src/config.rs crates/transfer/src/receiver.rs crates/transfer/src/generator.rs crates/transfer/src/tests/negotiated_algorithms.rs
git commit -m "refactor: add Default to ServerConfig, simplify test construction sites"
```

---

### Task 2: Extract `FileSelectionConfig`

**Files:**
- Modify: `crates/transfer/src/config.rs`
- Modify: `crates/transfer/src/receiver.rs` (access sites)

**Step 1: Define the sub-struct in `config.rs`**

```rust
/// File selection and filtering options.
///
/// Controls which files are candidates for transfer based on size,
/// existence at destination, and external file lists.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct FileSelectionConfig {
    /// Minimum file size in bytes. Files smaller than this are skipped.
    pub min_file_size: Option<u64>,
    /// Maximum file size in bytes. Files larger than this are skipped.
    pub max_file_size: Option<u64>,
    /// Skip updating files that already exist at the destination (`--ignore-existing`).
    pub ignore_existing: bool,
    /// Skip creating new files - only update existing files (`--existing`).
    pub existing_only: bool,
    /// Compare only file sizes, ignoring modification times (`--size-only`).
    pub size_only: bool,
    /// Path for `--files-from` when the server reads the file list directly.
    pub files_from_path: Option<String>,
    /// Use NUL bytes as delimiters for `--files-from` input (`--from0`).
    pub from0: bool,
}
```

**Step 2: Replace individual fields in `ServerConfig`**

Remove the 7 individual fields and add:

```rust
pub struct ServerConfig {
    // ... core fields ...
    /// File selection and filtering configuration.
    pub file_selection: FileSelectionConfig,
    // ... remaining fields ...
}
```

**Step 3: Update `Default` impl and `from_flag_string_and_args`**

In `Default`: replace 7 field initializers with `file_selection: FileSelectionConfig::default()`.

In `from_flag_string_and_args`: same replacement.

**Step 4: Update all access sites**

Find-and-replace pattern in receiver.rs and generator.rs:
- `self.config.min_file_size` -> `self.config.file_selection.min_file_size`
- `self.config.max_file_size` -> `self.config.file_selection.max_file_size`
- `self.config.ignore_existing` -> `self.config.file_selection.ignore_existing`
- `self.config.existing_only` -> `self.config.file_selection.existing_only`
- `self.config.size_only` -> `self.config.file_selection.size_only`
- `self.config.files_from_path` -> `self.config.file_selection.files_from_path`
- `self.config.from0` -> `self.config.file_selection.from0`

Also update construction sites in `crates/core/src/client/remote/`:
- `daemon_transfer.rs` (lines ~1028, ~1075)
- `ssh_transfer.rs` (lines ~540, ~562)
- `flags.rs` (lines ~176-177)

And `crates/cli/src/frontend/server.rs` where these fields are set after parsing.

**Step 5: Verify compilation**

Run: `cargo check --workspace --all-features`

**Step 6: Run tests**

Run: `cargo nextest run -p transfer --all-features -E 'test(config) | test(negotiated) | test(receiver) | test(generator)'`

**Step 7: Commit**

```bash
git add -u
git commit -m "refactor: extract FileSelectionConfig from ServerConfig"
```

---

### Task 3: Extract `WriteConfig`

**Files:**
- Modify: `crates/transfer/src/config.rs`
- Modify: `crates/transfer/src/receiver.rs`

**Step 1: Define the sub-struct**

```rust
/// File write behavior configuration.
///
/// Controls how the receiver writes transferred data to disk.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WriteConfig {
    /// Call fsync() after writing each file (`--fsync`).
    pub fsync: bool,
    /// Write directly to destination without temp-file + rename (`--inplace`).
    pub inplace: bool,
    /// Write data to device files instead of creating with mknod (`--write-devices`).
    pub write_devices: bool,
    /// Policy controlling io_uring usage for file I/O.
    pub io_uring_policy: fast_io::IoUringPolicy,
}

impl Default for WriteConfig {
    fn default() -> Self {
        Self {
            fsync: false,
            inplace: false,
            write_devices: false,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
        }
    }
}
```

Note: cannot `#[derive(Default)]` because `IoUringPolicy` may not impl `Default`.

**Step 2: Replace fields in `ServerConfig`**

Remove `fsync`, `inplace`, `write_devices`, `io_uring_policy`. Add `pub write: WriteConfig`.

**Step 3: Update access sites**

- `self.config.fsync` -> `self.config.write.fsync`
- `self.config.inplace` -> `self.config.write.inplace`
- `self.config.write_devices` -> `self.config.write.write_devices`
- `self.config.io_uring_policy` -> `self.config.write.io_uring_policy`

Also update `daemon_transfer.rs`, `ssh_transfer.rs`, `flags.rs`, `server.rs`.

**Step 4: Verify and test**

Run: `cargo check --workspace --all-features && cargo nextest run -p transfer --all-features -E 'test(config) | test(negotiated) | test(receiver) | test(generator)'`

**Step 5: Commit**

```bash
git add -u
git commit -m "refactor: extract WriteConfig from ServerConfig"
```

---

### Task 4: Extract `DeletionConfig`

**Files:**
- Modify: `crates/transfer/src/config.rs`
- Modify: `crates/transfer/src/receiver.rs`
- Modify: `crates/transfer/src/generator.rs`

**Step 1: Define the sub-struct**

```rust
/// Deletion behavior configuration.
///
/// Controls how extraneous file deletion is handled during transfer.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct DeletionConfig {
    /// Maximum number of deletions allowed (`--max-delete=NUM`).
    pub max_delete: Option<u64>,
    /// Delete files even if there are I/O errors (`--ignore-errors`).
    pub ignore_errors: bool,
}
```

**Step 2: Replace fields, update access sites**

- `self.config.max_delete` -> `self.config.deletion.max_delete`
- `self.config.ignore_errors` -> `self.config.deletion.ignore_errors`

Also update `daemon_transfer.rs` and `server.rs`.

**Step 3: Verify, test, commit**

```bash
git add -u
git commit -m "refactor: extract DeletionConfig from ServerConfig"
```

---

### Task 5: Extract `ConnectionConfig`

**Files:**
- Modify: `crates/transfer/src/config.rs`
- Modify: `crates/transfer/src/receiver.rs`
- Modify: `crates/transfer/src/generator.rs`

**Step 1: Define the sub-struct**

```rust
/// Connection and protocol context configuration.
///
/// Describes the connection type and associated protocol-level settings
/// that affect how file lists and filters are exchanged.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct ConnectionConfig {
    /// When true, indicates client-side operation (daemon client mode).
    pub client_mode: bool,
    /// Indicates the transfer is over a daemon (rsync://) connection.
    pub is_daemon_connection: bool,
    /// Filter rules to send to remote daemon (client_mode only).
    pub filter_rules: Vec<FilterRuleWireFormat>,
    /// Optional filename encoding converter for `--iconv` support.
    pub iconv: Option<FilenameConverter>,
    /// Optional compression level for zlib compression (0-9).
    pub compression_level: Option<CompressionLevel>,
}
```

**Step 2: Replace fields, update access sites**

- `self.config.client_mode` -> `self.config.connection.client_mode`
- `self.config.is_daemon_connection` -> `self.config.connection.is_daemon_connection`
- `self.config.filter_rules` -> `self.config.connection.filter_rules`
- `self.config.iconv` -> `self.config.connection.iconv`
- `self.config.compression_level` -> `self.config.connection.compression_level`

These fields are heavily used across both receiver.rs and generator.rs, so this task has the most access sites to update.

**Step 3: Verify, test, commit**

```bash
git add -u
git commit -m "refactor: extract ConnectionConfig from ServerConfig"
```

---

### Task 6: Final cleanup and verification

**Step 1: Verify final `ServerConfig` shape**

After all extractions, `ServerConfig` should look like:

```rust
pub struct ServerConfig {
    // Core identity (always needed, no meaningful defaults)
    pub role: ServerRole,
    pub protocol: ProtocolVersion,
    pub flag_string: String,
    pub flags: ParsedServerFlags,
    pub args: Vec<OsString>,

    // Grouped sub-configs
    pub file_selection: FileSelectionConfig,
    pub write: WriteConfig,
    pub deletion: DeletionConfig,
    pub connection: ConnectionConfig,

    // Remaining ungrouped fields (too few to warrant a sub-struct)
    pub reference_directories: Vec<ReferenceDirectory>,
    pub checksum_seed: Option<u32>,
    pub checksum_choice: Option<protocol::ChecksumAlgorithm>,
    pub trust_sender: bool,
    pub stop_at: Option<SystemTime>,
    pub qsort: bool,
}
```

27 fields reduced to 6 ungrouped + 4 sub-configs (containing 18 fields total) = 10 top-level entries.

**Step 2: Run full workspace check**

Run: `cargo check --workspace --all-features`
Run: `cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings`
Run: `cargo fmt --all -- --check`

**Step 3: Run transfer tests**

Run: `cargo nextest run -p transfer --all-features`

**Step 4: Run CLI tests (server.rs uses ServerConfig)**

Run: `cargo nextest run -p cli --all-features`

**Step 5: Commit any formatting fixes**

```bash
cargo fmt --all
git add -u
git commit -m "style: format after ServerConfig decomposition"
```

---

## Risk Mitigation

- **Each task is independently compilable** - if any extraction causes issues, it can be reverted without affecting others.
- **Task 1 (Default) is the highest-value change** - even alone, it eliminates the "27-field enumeration" pain in all test sites.
- **Sub-config `Default` impls** match the current field defaults exactly - no behavioral change.
- **Access site updates are mechanical** - `self.config.field` becomes `self.config.group.field`. No logic changes.

## Files Modified (Summary)

| File | Tasks |
|------|-------|
| `crates/transfer/src/config.rs` | 1, 2, 3, 4, 5, 6 |
| `crates/transfer/src/receiver.rs` | 1, 2, 3, 4, 5 |
| `crates/transfer/src/generator.rs` | 1, 2, 3, 4, 5 |
| `crates/transfer/src/tests/negotiated_algorithms.rs` | 1 |
| `crates/transfer/tests/incremental_transfer.rs` | 1 |
| `crates/core/src/client/remote/daemon_transfer.rs` | 2, 3, 4, 5 |
| `crates/core/src/client/remote/ssh_transfer.rs` | 2, 3 |
| `crates/core/src/client/remote/flags.rs` | 2, 3 |
| `crates/cli/src/frontend/server.rs` | 2, 3, 4, 5 |
