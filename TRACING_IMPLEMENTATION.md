# Rsync Tracing/Debugging Implementation Summary

This document summarizes the implementation of rsync-compatible tracing and debugging output in the Rust rsync implementation.

## Overview

The implementation provides full compatibility with rsync's verbosity system while integrating with Rust's `tracing` ecosystem. Users can use standard rsync flags (`-v`, `--info`, `--debug`) and developers can use standard Rust tracing macros.

## Architecture

### Components

1. **Logging Crate** (`crates/logging/`)
   - Core verbosity configuration system
   - Info and debug flag definitions
   - Thread-local verbosity state
   - Tracing integration layer (feature-gated)

2. **Logging-Sink Crate** (`crates/logging-sink/`)
   - Message output and formatting
   - Rsync-compatible message rendering

3. **CLI Integration** (`crates/cli/`)
   - Argument parsing for `-v`, `--info`, `--debug`
   - Verbosity configuration initialization
   - Tracing subscriber setup

### Key Files

| File | Purpose |
|------|---------|
| `crates/logging/src/config.rs` | Verbosity configuration and level mapping |
| `crates/logging/src/levels.rs` | Info and debug flag enums |
| `crates/logging/src/tracing_bridge.rs` | Tracing integration layer |
| `crates/logging/src/tracing_macros.rs` | Convenience macros for tracing |
| `crates/logging/TRACING.md` | User documentation |
| `crates/cli/src/frontend/execution/flags.rs` | CLI flag parsing |

## Features

### Verbosity Levels

The implementation supports 6 verbosity levels (0-5+), matching upstream rsync:

- **Level 0**: Minimal output (errors only)
- **Level 1** (`-v`): Basic file transfer information
- **Level 2** (`-vv`): Detailed file information + basic debug flags
- **Level 3** (`-vvv`): Protocol debugging
- **Level 4** (`-vvvv`): Extensive debugging
- **Level 5+** (`-vvvvv`): Maximum verbosity including I/O and hash operations

### Info Flags

13 info flags for controlling informational output:

- `backup`, `copy`, `del`, `flist`, `misc`, `mount`, `name`, `nonreg`, `progress`, `remove`, `skip`, `stats`, `symsafe`

Each flag supports multiple levels (typically 0-2 or 0-3).

### Debug Flags

24 debug flags for detailed diagnostic output:

- `acl`, `backup`, `bind`, `chdir`, `cmd`, `connect`, `del`, `deltasum`, `dup`, `exit`, `filter`, `flist`, `fuzzy`, `genr`, `hash`, `hlink`, `iconv`, `io`, `nstr`, `own`, `proto`, `recv`, `send`, `time`

Most flags support levels 0-4 for increasingly detailed output.

## Tracing Integration

### Rust Tracing Bridge

The `tracing_bridge` module provides a custom `tracing-subscriber` layer that:

1. Intercepts tracing events based on target
2. Maps targets to rsync info/debug flags
3. Checks verbosity configuration
4. Emits events through rsync's diagnostic system

### Target Mapping

Tracing targets are automatically mapped to rsync flags:

```rust
// Info flags
"rsync::copy" → InfoFlag::Copy
"rsync::delete" → InfoFlag::Del
"rsync::stats" → InfoFlag::Stats

// Debug flags
"rsync::protocol" → DebugFlag::Proto
"rsync::delta" → DebugFlag::Deltasum
"rsync::receiver" → DebugFlag::Recv
"rsync::io" → DebugFlag::Io
```

### Level Mapping

Tracing levels map to verbosity levels:

- `ERROR`, `WARN`, `INFO` → Level 1
- `DEBUG` → Level 2
- `TRACE` → Level 3

## Usage

### For Users

```bash
# Basic verbosity
oc-rsync -v src/ dest/

# Multiple levels
oc-rsync -vvv src/ dest/

# Specific info flags
oc-rsync --info=progress2,stats src/ dest/

# Specific debug flags
oc-rsync --debug=proto,recv src/ dest/

# Combined
oc-rsync -vv --info=name2 --debug=deltasum2 src/ dest/

# Help
oc-rsync --info=help
oc-rsync --debug=help
```

### For Developers

```rust
use tracing::{debug, info, trace};

// Standard tracing macros with appropriate targets
info!(target: "rsync::copy", "copying file: {}", path);
debug!(target: "rsync::protocol", "negotiated version {}", version);
trace!(target: "rsync::io", "read {} bytes", count);

// Or use convenience macros
use logging::{trace_copy, trace_proto, trace_io};

trace_copy!("copying file: {}", path);
trace_proto!("negotiated version {}", version);
trace_io!("read {} bytes", count);

// Initialize tracing with verbosity config
use logging::{VerbosityConfig, init_tracing};

let config = VerbosityConfig::from_verbose_level(2);
init_tracing(config);
```

## Testing

The implementation includes comprehensive tests:

1. **Unit Tests**: Test flag mapping, level conversion, config parsing
2. **Integration Tests**: Test verbosity progression, event ordering, filtering
3. **Example**: `crates/logging/examples/tracing_demo.rs`

Run tests:

```bash
cargo test --package logging --features tracing
```

## Compatibility

### With Upstream Rsync

- ✅ Verbosity levels (`-v`, `-vv`, `-vvv`) match rsync 3.4.1 behavior
- ✅ Info flag names and levels match upstream
- ✅ Debug flag names and levels match upstream
- ✅ `--info=help` and `--debug=help` output compatibility
- ✅ Output format matches rsync (where applicable)

### With Rust Ecosystem

- ✅ Compatible with standard `tracing` crate
- ✅ Works with `tracing-subscriber` layers
- ✅ Can combine with env filters (`RUST_LOG`)
- ✅ Zero-cost when features disabled

## Performance

- Thread-local storage for fast verbosity checks
- No heap allocation in hot paths
- Conditional compilation with feature flags
- Event collection can be disabled per flag/level

## Future Enhancements

Potential improvements for future versions:

1. **Structured Logging**: Export events in JSON format
2. **Real-time Filtering**: Dynamic verbosity adjustment
3. **Performance Profiling**: Integrate with `tracing-flame`
4. **Log Aggregation**: Send events to external systems
5. **Custom Formatters**: User-defined output formats

## Documentation

- **User Guide**: `crates/logging/TRACING.md`
- **API Documentation**: `cargo doc --package logging --open`
- **Examples**: `crates/logging/examples/`
- **Integration Tests**: `crates/logging/src/tracing_bridge_tests.rs`

## Dependencies

The tracing feature adds these dependencies:

- `tracing`: 0.1 (core tracing primitives)
- `tracing-subscriber`: 0.3 (subscriber implementation)

These are workspace dependencies already used by other crates.

## Configuration

Enable tracing support in `Cargo.toml`:

```toml
[dependencies]
logging = { path = "../logging", features = ["tracing"] }
```

The CLI crate enables this by default to provide full tracing integration.

## Examples

### Basic Transfer with Progress

```bash
oc-rsync -v --info=progress2 large_dir/ backup/
```

Output:
```
file1.txt
file2.dat
          1,234,567  50%   123.45kB/s    0:00:08
sending incremental file list
file3.log

sent 1.5M bytes  received 2.3K bytes  234.5K bytes/sec
total size is 12.3M  speedup is 8.2
```

### Debug Protocol Negotiation

```bash
oc-rsync -vvv --debug=proto2 src/ remote:/dest/
```

Output:
```
[Proto] opening connection using: ssh remote rsync --server -vvv . /dest/
[Proto] sending protocol version 31
[Proto] received protocol version 31
[Proto] negotiated protocol version 31
[Proto] checksum: auto (using xxh128)
[Proto] compress: zstd (level 3)
...
```

### Performance Analysis

```bash
oc-rsync --info=stats3 --debug=io2,deltasum2 src/ dest/
```

Shows detailed statistics about I/O operations and delta computations.

## Migration Guide

### From Old Logging System

The old system used direct message emission. New code should use tracing macros:

```rust
// Old
info_log!(Copy, 1, "copying {}", path);

// New
tracing::info!(target: "rsync::copy", "copying {}", path);
// or
trace_copy!("copying {}", path);
```

Both systems work concurrently during migration.

### Adding Tracing to Existing Code

1. Add tracing dependency
2. Choose appropriate target for the module
3. Use tracing macros instead of println!/eprintln!
4. Let the verbosity system handle filtering

## Troubleshooting

### Events Not Appearing

- Check verbosity level with `-v` or higher
- Verify correct target (use `rsync::` prefix)
- Ensure tracing is initialized before events
- Check flag levels in `VerbosityConfig`

### Too Much Output

- Reduce verbosity level
- Disable specific flags: `--info=nocopy,nomisc`
- Use `--quiet` to suppress all output

### Integration Issues

- Ensure `tracing` feature is enabled
- Check that `init_tracing()` is called early
- Verify workspace dependency versions match

## References

- [Upstream rsync debugging](https://rsync.samba.org/ftp/rsync/rsync.html#debug)
- [Rust tracing documentation](https://docs.rs/tracing/)
- [tracing-subscriber guide](https://docs.rs/tracing-subscriber/)
- [Rsync protocol specification](https://rsync.samba.org/tech_report/)

## Authors

Implementation by the Rust rsync team, based on rsync 3.4.1 behavior.

## License

GPL-3.0-or-later (matching upstream rsync)
