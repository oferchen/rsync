# Batch Mode Implementation

## Overview

Batch mode allows recording an rsync transfer operation to a file and replaying it later. This is useful for:
- Distributing the same changes to multiple destinations
- Transferring when source and destination are not simultaneously available
- Creating repeatable transfer operations
- Offline/disconnected scenarios

## Current Status

✅ **Complete**: Core batch crate (`crates/batch/src/`)
- Binary format matching upstream rsync
- BatchWriter for recording transfers
- BatchReader for replaying transfers
- Shell script generation (.sh files)
- Full test coverage (~50 tests passing)
- CLI integration (--write-batch, --only-write-batch, --read-batch)
- Transfer I/O integration in `crates/core/src/client/run.rs`

## Architecture

### Module Structure

```
crates/batch/src/
├── lib.rs           # Public API and BatchConfig
├── error.rs         # Error types
├── format.rs        # Binary format (BatchHeader, BatchFlags)
├── writer.rs        # BatchWriter implementation
├── reader.rs        # BatchReader implementation
├── script.rs        # Shell script generation
└── tests.rs         # Integration tests
```

> **Note**: The `engine` crate re-exports batch types for convenience
> (`engine::batch::*`), but the source of truth is the `batch` crate.

### Batch File Format

The batch file format maintains byte-for-byte compatibility with upstream rsync:

1. **Header**:
   - Protocol version (i32, little-endian)
   - Compat flags (varint for protocol >= 30)
   - Checksum seed (i32, little-endian)
   - Stream flags bitmap (i32, see below)

2. **Stream Flags** (protocol-dependent):
   - Bit 0: `--recurse` (-r)
   - Bit 1: `--owner` (-o)
   - Bit 2: `--group` (-g)
   - Bit 3: `--links` (-l)
   - Bit 4: `--devices` (-D)
   - Bit 5: `--hard-links` (-H)
   - Bit 6: `--checksum` (-c)
   - Bit 7: `--dirs` (-d) [protocol >= 29]
   - Bit 8: `--compress` (-z) [protocol >= 29]
   - Bit 9: `--iconv` [protocol >= 30]
   - Bit 10: `--acls` (-A) [protocol >= 30]
   - Bit 11: `--xattrs` (-X) [protocol >= 30]
   - Bit 12: `--inplace` [protocol >= 30]
   - Bit 13: `--append` [protocol >= 30]
   - Bit 14: `--append-verify` [protocol >= 30]

3. **File List**: Standard flist encoding (same as network protocol)

4. **Delta Operations**: Copy and literal tokens for each file

5. **Statistics**: Transfer metrics (at end)

### Shell Script Format

The `.sh` script file contains:
```bash
#!/bin/sh
rsync [options] --read-batch=BATCHFILE ${1:-DEST}
```

If filter rules are present, they're embedded using a heredoc:
```bash
rsync [options] --read-batch=BATCHFILE ${1:-DEST} <<'#E#'
- *.tmp
+ */
- *
#E#
```

## API Usage

### Writing a Batch

```rust
use engine::batch::{BatchConfig, BatchMode, BatchWriter, BatchFlags};

// Create configuration
let config = BatchConfig::new(
    BatchMode::Write,           // or BatchMode::OnlyWrite
    "mybatch".to_string(),
    30,                          // protocol version
).with_checksum_seed(12345);

// Create writer
let mut writer = BatchWriter::new(config)?;

// Write header with stream flags
let mut flags = BatchFlags::default();
flags.recurse = true;
flags.preserve_uid = true;
flags.compress = true;
writer.write_header(flags)?;

// During transfer, write file list and delta data
writer.write_data(&file_list_bytes)?;
writer.write_data(&delta_bytes)?;

// Finalize
writer.finalize()?;
```

### Reading a Batch

```rust
use engine::batch::{BatchConfig, BatchMode, BatchReader};

// Create configuration
let config = BatchConfig::new(
    BatchMode::Read,
    "mybatch".to_string(),
    30,
);

// Create reader
let mut reader = BatchReader::new(config)?;

// Read and validate header
let flags = reader.read_header()?;

// Read file list and delta data
let mut buf = vec![0u8; 4096];
loop {
    let n = reader.read_data(&mut buf)?;
    if n == 0 { break; }
    // Process data...
}
```

### Generating Shell Script

```rust
use engine::batch::script::generate_script;

let args = vec![
    "oc-rsync".to_string(),
    "-av".to_string(),
    "--write-batch=mybatch".to_string(),
    "source/".to_string(),
    "dest/".to_string(),
];

// Optional filter rules
let filter_rules = Some("- *.tmp\n+ */\n- *\n");

generate_script(&config, &args, filter_rules)?;
// Creates mybatch.sh (executable)
```

## Integration Points

### 1. CLI Argument Handling

The CLI already parses batch arguments in `crates/cli/src/frontend/arguments/parser.rs`:
- `--write-batch=FILE`
- `--only-write-batch=FILE`
- `--read-batch=FILE`

These are stored in `ParsedArgs`:
```rust
pub struct ParsedArgs {
    // ...
    pub write_batch: Option<OsString>,
    pub only_write_batch: Option<OsString>,
    pub read_batch: Option<OsString>,
    // ...
}
```

### 2. Transfer I/O Integration

Batch mode is integrated in `crates/core/src/client/run.rs`:
- **Write mode**: `BatchWriter::new()` is created when `config.batch_config()` is set, writes header, attaches to transfer options
- **Read mode**: `replay_batch()` is called to replay a previously recorded batch

### 3. Protocol Coordination

Batch files must use the same protocol version as the transfer. The protocol version is determined during negotiation and should be passed to `BatchConfig::new()`.

### 5. Error Handling

Batch operations should emit errors with the `[client]` role trailer and exit code 1 for I/O errors, 23 for partial transfers.

## Testing

### Unit Tests (~50 tests, all passing)

```bash
cargo nextest run -p batch
```

Coverage includes:
- Binary format serialization/deserialization
- Protocol version compatibility (28, 29, 30+)
- Round-trip data integrity
- Large data handling
- Corruption detection
- Script generation
- Shell quoting

### Integration Tests

End-to-end interop tests verify batch file interchangeability with upstream rsync 3.0.9, 3.1.3, and 3.4.1.

## Usage Examples

### Write a Batch During Transfer

```bash
# Perform transfer and record batch
oc-rsync -av --write-batch=updates source/ dest/

# Two files created:
# - updates (binary batch file)
# - updates.sh (replay script)
```

### Only Write Batch (No Transfer)

```bash
# Record batch without modifying destination
oc-rsync -av --only-write-batch=updates source/ dest/
```

### Read and Replay Batch

```bash
# Method 1: Use generated script
./updates.sh /actual/dest/

# Method 2: Manual replay
oc-rsync --read-batch=updates /actual/dest/
```

### Distribute to Multiple Destinations

```bash
# Create batch once
oc-rsync -av --only-write-batch=updates source/ dest/

# Replay to multiple destinations
for dest in server1:/data server2:/data server3:/data; do
    ./updates.sh $dest
done
```

## Limitations

1. **Local transfers only** (currently): Batch mode requires integration with remote transport for rsync:// and ssh:// URLs
2. **No compression of batch file**: The batch file itself is not compressed (matches upstream)
3. **Protocol version must match**: Reading a batch requires the same protocol version it was written with
4. **File list replay**: When reading a batch, the source directory is not accessed (file list comes from batch)

## Future Enhancements

1. **Batch file compression**: Add `--compress-batch` option to gzip the batch file
2. **Incremental batch updates**: Support updating an existing batch with new changes
3. **Batch merging**: Combine multiple batch files into one
4. **Remote batch**: Support `--write-batch` with remote sources/destinations
5. **Batch validation**: Add `--verify-batch` to check batch file integrity without applying

## Upstream Compatibility

The implementation follows upstream rsync's batch.c implementation (rsync 3.4.1):
- Binary format matches byte-for-byte
- Stream flags bitmap identical
- Shell script format compatible
- Protocol version handling identical

Tested against: rsync 3.0.9, 3.1.3, 3.4.1

## Implementation References

- Upstream: `target/interop/upstream-src/rsync-3.4.1/batch.c`
- Format definitions: `crates/batch/src/format.rs`
- Writer: `crates/batch/src/writer.rs`
- Reader: `crates/batch/src/reader.rs`
- Script generator: `crates/batch/src/script.rs`

## See Also

- Upstream rsync batch mode documentation: `man rsync` (search for "batch")
- Protocol specification: `docs/PROTOCOL.md` (file list and delta encoding)
- Batch crate API: `crates/batch/src/lib.rs`
