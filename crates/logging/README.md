# Logging Crate

Rsync-compatible logging and verbosity flag system with optional tracing integration.

## Overview

This crate provides the logging infrastructure for the Rust rsync implementation. It supports:

- **Rsync-compatible verbosity levels** (`-v`, `-vv`, `-vvv`)
- **Fine-grained info flags** (13 flags, matching upstream rsync)
- **Detailed debug flags** (24 flags, matching upstream rsync)
- **Optional tracing integration** (feature-gated)
- **Thread-local state** for zero-cost checks
- **Zero dependencies** in default mode

## Features

### Default (No Features)

The crate is intentionally dependency-free by default to avoid circular dependencies:

```toml
[dependencies]
logging = { path = "../logging" }
```

This provides:
- Verbosity configuration (`VerbosityConfig`)
- Info/debug flag enums and levels
- Thread-local state management
- Diagnostic event collection
- Logging macros (`info_log!`, `debug_log!`)

### Tracing Feature

Enable tracing integration for bridging with Rust's `tracing` ecosystem:

```toml
[dependencies]
logging = { path = "../logging", features = ["tracing"] }
```

This adds:
- `RsyncLayer` - tracing-subscriber layer
- `init_tracing()` - initialize tracing with rsync config
- Convenience macros (`trace_copy!`, `trace_proto!`, etc.)
- Automatic target → flag mapping

### Serde Feature

Enable serialization support:

```toml
[dependencies]
logging = { path = "../logging", features = ["serde"] }
```

This allows serializing/deserializing verbosity configurations.

## Quick Start

### Basic Usage (No Tracing)

```rust
use logging::{VerbosityConfig, InfoFlag, DebugFlag, info_log, debug_log};

// Initialize verbosity from -vv flag
logging::init(VerbosityConfig::from_verbose_level(2));

// Use logging macros
info_log!(Copy, 1, "copying {}", path);
debug_log!(Deltasum, 2, "computed {} blocks", count);

// Check if logging is enabled before expensive operations
if logging::info_gte(InfoFlag::Copy, 1) {
    let details = compute_expensive_details();
    logging::emit_info(InfoFlag::Copy, 1, details);
}
```

### With Tracing

```rust
use logging::{VerbosityConfig, init_tracing};

// Initialize tracing with rsync verbosity
let config = VerbosityConfig::from_verbose_level(2);
init_tracing(config);

// Use standard tracing macros
tracing::info!(target: "rsync::copy", "copying {}", path);
tracing::debug!(target: "rsync::delta", "computed {} blocks", count);

// Or use convenience macros
logging::trace_copy!("copying {}", path);
logging::trace_delta!("computed {} blocks", count);
```

### Manual Flag Configuration

```rust
use logging::VerbosityConfig;

let mut config = VerbosityConfig::default();

// Apply specific flags
config.apply_info_flag("copy2").unwrap();
config.apply_info_flag("progress2").unwrap();
config.apply_debug_flag("proto").unwrap();

logging::init(config);
```

## Verbosity Levels

The `VerbosityConfig::from_verbose_level(n)` function maps verbosity levels to specific flag configurations:

| Level | Description | Enabled Flags |
|-------|-------------|---------------|
| 0 | Quiet | `nonreg=1` only |
| 1 | Basic (`-v`) | Basic info flags |
| 2 | Detailed (`-vv`) | Info flags + basic debug |
| 3 | Protocol (`-vvv`) | Enhanced debug |
| 4 | Extensive (`-vvvv`) | Maximum debug |
| 5+ | Maximum (`-vvvvv`) | All flags including I/O |

See `crates/logging/src/config.rs` for exact mappings.

## Info Flags

13 informational message categories:

- `backup` - Backup file operations
- `copy` - File copy operations
- `del` - File deletion operations
- `flist` - File list building (levels 0-2)
- `misc` - Miscellaneous information (levels 0-2)
- `mount` - Mount point warnings
- `name` - File names (0=off, 1=changed, 2=all)
- `nonreg` - Non-regular file handling
- `progress` - Transfer progress (0=off, 1=per-file, 2=overall)
- `remove` - File removal operations
- `skip` - Skipped files (levels 0-2)
- `stats` - Transfer statistics (levels 0-3)
- `symsafe` - Symlink safety warnings

## Debug Flags

24 debug diagnostic categories:

- `acl` - ACL processing
- `backup` - Backup file creation
- `bind` - Socket binding
- `chdir` - Directory changes
- `cmd` - Command execution
- `connect` - Connection establishment
- `del` - Deletion operations (levels 0-3)
- `deltasum` - Delta computation (levels 0-4)
- `dup` - Duplicate detection
- `exit` - Exit status
- `filter` - Filter rule processing
- `flist` - File list operations (levels 0-4)
- `fuzzy` - Fuzzy basis matching
- `genr` - Generator operations
- `hash` - Hash calculations
- `hlink` - Hard link detection
- `iconv` - Character encoding
- `io` - I/O operations (levels 0-4)
- `nstr` - Name string operations
- `own` - Ownership changes
- `proto` - Protocol negotiation
- `recv` - Receiver operations
- `send` - Sender operations
- `time` - Timing information

## Tracing Integration

### Target Mapping

Tracing targets are mapped to rsync flags using naming conventions:

```rust
// Info flags
"rsync::copy" -> InfoFlag::Copy
"rsync::delete" -> InfoFlag::Del
"rsync::stats" -> InfoFlag::Stats

// Debug flags
"rsync::protocol" -> DebugFlag::Proto
"rsync::delta" -> DebugFlag::Deltasum
"rsync::receiver" -> DebugFlag::Recv
"rsync::io" -> DebugFlag::Io
```

### Level Mapping

Tracing levels map to verbosity levels:

- `ERROR`, `WARN`, `INFO` → Verbosity level 1
- `DEBUG` → Verbosity level 2
- `TRACE` → Verbosity level 3

### Convenience Macros

When the `tracing` feature is enabled:

```rust
trace_copy!("copying {}", path);       // -> info!(target: "rsync::copy")
trace_del!("deleting {}", path);       // -> info!(target: "rsync::delete")
trace_stats!("transferred {}", bytes); // -> info!(target: "rsync::stats")
trace_proto!("version {}", ver);       // -> debug!(target: "rsync::protocol")
trace_delta!("blocks {}", count);      // -> debug!(target: "rsync::delta")
trace_recv!("received {}", bytes);     // -> debug!(target: "rsync::receiver")
trace_send!("sending {}", path);       // -> debug!(target: "rsync::sender")
trace_io!("read {} bytes", n);         // -> trace!(target: "rsync::io")
trace_connect!("connecting");          // -> debug!(target: "rsync::connect")
trace_filter!("rule {}", rule);        // -> debug!(target: "rsync::filter")
trace_genr!("generating {}", path);    // -> debug!(target: "rsync::generator")
```

## Architecture

### Thread-Local State

Verbosity configuration is stored in thread-local storage for fast access:

```rust
thread_local! {
    static VERBOSITY: RefCell<VerbosityConfig> = ...;
    static EVENTS: RefCell<Vec<DiagnosticEvent>> = ...;
}
```

This allows zero-cost checks:

```rust
if info_gte(InfoFlag::Copy, 1) {
    // Fast thread-local lookup, no synchronization
    emit_info(InfoFlag::Copy, 1, message);
}
```

### Event Collection

Diagnostic events can be collected and drained:

```rust
logging::emit_info(InfoFlag::Copy, 1, "test".to_owned());
let events = logging::drain_events();

for event in events {
    match event {
        DiagnosticEvent::Info { flag, level, message } => { ... }
        DiagnosticEvent::Debug { flag, level, message } => { ... }
    }
}
```

## Testing

Run tests with:

```bash
# Basic tests
cargo test --package logging

# With tracing feature
cargo test --package logging --features tracing

# All features
cargo test --package logging --all-features
```

## Examples

See `crates/logging/examples/` for complete examples:

- `tracing_demo.rs` - Demonstrates tracing integration

Run with:

```bash
cargo run --package logging --example tracing_demo --features tracing
```

## Documentation

- **API Docs**: Run `cargo doc --package logging --open`
- **User Guide**: See `TRACING.md` for end-user documentation
- **Quick Reference**: See `QUICK_REFERENCE.md` for command-line flags
- **Implementation**: See `/TRACING_IMPLEMENTATION.md` for architecture details

## Performance

- Thread-local storage: ~5ns per check (near zero cost)
- No heap allocation in hot paths
- Conditional compilation: zero cost when features disabled
- Event collection can be disabled per flag/level

## Compatibility

Designed to match rsync 3.4.1 behavior:

- ✅ Verbosity level mapping matches upstream
- ✅ Info flag names and semantics match upstream
- ✅ Debug flag names and semantics match upstream
- ✅ Help text format matches upstream
- ✅ Output format compatible with parsers

## Contributing

When adding new flags:

1. Add enum variant to `InfoFlag` or `DebugFlag` in `levels.rs`
2. Add field to `InfoLevels` or `DebugLevels` in `levels.rs`
3. Update `from_verbose_level()` in `config.rs` to include in appropriate levels
4. Add parsing case in `apply_info_flag()` or `apply_debug_flag()` in `config.rs`
5. Add target mapping in `tracing_bridge.rs` if using tracing feature
6. Update documentation in `TRACING.md` and `QUICK_REFERENCE.md`
7. Add tests

## License

GPL-3.0-or-later (matching upstream rsync)
