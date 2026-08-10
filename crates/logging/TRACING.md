# Rsync Tracing and Debugging Output

This document describes the tracing and debugging capabilities in the Rust rsync implementation, which are designed to be compatible with upstream rsync's `-v`, `-vv`, `-vvv`, `--info`, and `--debug` flags.

## Overview

The logging system provides three complementary approaches:

1. **Verbosity Levels** (`-v`, `-vv`, `-vvv`): Progressive information disclosure
2. **Info Flags** (`--info=FLAGS`): Fine-grained control over informational messages
3. **Debug Flags** (`--debug=FLAGS`): Detailed diagnostic output for debugging

## Verbosity Levels

### Basic Usage

```bash
# No verbosity - minimal output (errors only)
oc-rsync src/ dest/

# Level 1 (-v): Basic transfer information
oc-rsync -v src/ dest/
# Shows: file names being transferred, transfer statistics

# Level 2 (-vv): Detailed file information
oc-rsync -vv src/ dest/
# Shows: what -v shows plus file attributes, skipped files, debug flags activated

# Level 3 (-vvv): Protocol debug information
oc-rsync -vvv src/ dest/
# Shows: what -vv shows plus protocol details, connection info, internal state

# Level 4 (-vvvv): Extensive debugging
oc-rsync -vvvv src/ dest/
# Shows: what -vvv shows plus detailed protocol debugging

# Level 5+ (-vvvvv): Maximum verbosity
oc-rsync -vvvvv src/ dest/
# Shows: everything including low-level I/O and hash computations
```

### Verbosity Mapping

Each verbosity level automatically enables specific info and debug flags:

| Level | Info Flags Enabled | Debug Flags Enabled |
|-------|-------------------|---------------------|
| 0 | `nonreg=1` | None |
| 1 | `copy, del, flist, misc, name, stats, symsafe` | None |
| 2 | Level 1 + `backup, mount, remove, skip` (level 2) | `bind, cmd, connect, del, deltasum, dup, filter, flist, iconv` |
| 3 | Level 2 | Level 2 + `acl, backup, fuzzy, genr, own, recv, send, time, exit` (enhanced) |
| 4 | Level 3 | Level 3 + enhanced protocol debugging |
| 5+ | Level 4 | Level 4 + `chdir, hash, hlink` |

## Info Flags (`--info=FLAGS`)

Info flags control informational messages. Use `--info=help` to see all available flags.

### Available Info Flags

- **`backup`**: Backup file operations
- **`copy`**: File copy operations (created/updated files)
- **`del`**: File deletion operations
- **`flist`**: File list building and transmission (levels 0-2)
- **`misc`**: Miscellaneous information (levels 0-2)
- **`mount`**: Mount point warnings
- **`name`**: File names being processed (0=off, 1=changed, 2=all)
- **`nonreg`**: Non-regular file handling
- **`progress`**: Transfer progress (0=off, 1=per-file, 2=overall)
- **`remove`**: File removal operations
- **`skip`**: Skipped files (levels 0-2)
- **`stats`**: Transfer statistics (levels 0-3)
- **`symsafe`**: Symlink safety warnings

### Info Flag Syntax

```bash
# Enable specific flags
--info=copy,del,stats

# Set flag level (higher = more verbose)
--info=name2        # Show all files (not just changed)
--info=stats3       # Maximum statistics detail

# Disable specific flags
--info=nocopy       # or -copy or copy0
--info=progress0

# Special keywords
--info=all          # Enable all info flags at level 1
--info=none         # Disable all info output

# Combine with verbosity
oc-rsync -v --info=progress2 src/ dest/
```

### Common Info Flag Combinations

```bash
# Progress indicator only (no file names)
oc-rsync --info=progress2,name0 src/ dest/

# Statistics summary only
oc-rsync --info=stats,name0 src/ dest/

# Show all files (including unchanged)
oc-rsync -v --info=name2 src/ dest/

# Deletion tracking
oc-rsync --delete --info=del,remove src/ dest/
```

## Debug Flags (`--debug=FLAGS`)

Debug flags provide detailed diagnostic output for development and troubleshooting. Use `--debug=help` to see all available flags.

### Available Debug Flags

- **`acl`**: ACL processing
- **`backup`**: Backup file creation
- **`bind`**: Socket binding
- **`chdir`**: Directory changes
- **`cmd`**: Command execution
- **`connect`**: Connection establishment (levels 0-2)
- **`del`**: Deletion operations (levels 0-3)
- **`deltasum`**: Delta computation (levels 0-4)
- **`dup`**: Duplicate detection
- **`exit`**: Exit status and cleanup (levels 0-3)
- **`filter`**: Filter rule processing (levels 0-2)
- **`flist`**: File list operations (levels 0-4)
- **`fuzzy`**: Fuzzy basis file matching
- **`genr`**: Generator operations
- **`hash`**: Hash calculations
- **`hlink`**: Hard link detection
- **`iconv`**: Character encoding conversion (levels 0-2)
- **`io`**: I/O operations (levels 0-4)
- **`nstr`**: Name string operations
- **`own`**: Ownership changes (levels 0-2)
- **`proto`**: Protocol negotiation (levels 0-2)
- **`recv`**: Receiver operations
- **`send`**: Sender operations
- **`time`**: Timing information (levels 0-2)

### Debug Flag Syntax

```bash
# Enable specific debug flags
--debug=recv,send,proto

# Set debug level
--debug=flist2      # Moderate file list debugging
--debug=io4         # Maximum I/O debugging

# Disable specific flags
--debug=noproto     # or -proto or proto0

# Special keywords
--debug=all         # Enable all debug flags at level 1
--debug=none        # Disable all debug output
```

### Common Debug Scenarios

```bash
# Debug protocol negotiation
oc-rsync --debug=proto,connect src/ remote:/dest/

# Debug file list issues
oc-rsync --debug=flist2,filter src/ dest/

# Debug performance (I/O and delta)
oc-rsync --debug=io,deltasum,recv,send src/ dest/

# Debug transfer failures
oc-rsync --debug=recv,send,exit src/ dest/

# Full protocol debugging
oc-rsync -vvv --debug=proto2,io2 src/ dest/
```

## Using Tracing in Code

When writing Rust code for rsync, you can use standard `tracing` macros with appropriate targets:

```rust
use tracing::{debug, info, trace};

// Copy operations
info!(target: "rsync::copy", "copying {}", path);

// Protocol debugging
debug!(target: "rsync::protocol", "negotiated version {}", version);

// I/O operations
trace!(target: "rsync::io", "read {} bytes", count);

// Or use convenience macros
use logging::{trace_copy, trace_proto, trace_io};

trace_copy!("copying {}", path);
trace_proto!("negotiated version {}", version);
trace_io!("read {} bytes", count);
```

### Target Naming Convention

Tracing targets are automatically mapped to rsync flags:

- `rsync::copy` → `InfoFlag::Copy`
- `rsync::delta` → `DebugFlag::Deltasum`
- `rsync::receiver` → `DebugFlag::Recv`
- `rsync::protocol` → `DebugFlag::Proto`
- `rsync::io` → `DebugFlag::Io`

### Tracing Levels

Tracing levels map to verbosity levels:

- `ERROR`, `WARN`, `INFO` → Level 1
- `DEBUG` → Level 2
- `TRACE` → Level 3

## Environment Variables

### Standard Tracing

```bash
# Enable all tracing (useful for development)
RUST_LOG=debug oc-rsync src/ dest/

# Filter by module
RUST_LOG=rsync::protocol=debug,rsync::io=trace oc-rsync src/ dest/

# Combine with rsync verbosity
RUST_LOG=debug oc-rsync -vv src/ dest/
```

## Output Format

Output matches rsync's format for compatibility:

```
# Info messages
rsync: copying file.txt
rsync: deleting obsolete.txt

# Debug messages
[Receiver] received 4096 bytes
[Proto] negotiated protocol version 31
[Delta] computed 128 blocks for file.dat

# Statistics (with --info=stats)
Number of files: 123 (reg: 100, dir: 20, link: 3)
Total file size: 1.23M bytes
Literal data: 456K
Matched data: 789K

sent 100.5K bytes  received 89.2K bytes  126.5K bytes/sec
total size is 1.23M  speedup is 6.49
```

## Troubleshooting

### No output with -v

Check that:
- You're not using `--quiet`
- You haven't disabled output with `--info=none`
- Files are actually being transferred (use `--itemize-changes`)

### Too much output

Reduce verbosity or disable specific flags:
```bash
# Reduce from -vvv to -vv
oc-rsync -vv src/ dest/

# Disable noisy flags
oc-rsync -v --info=nomisc,noskip src/ dest/
```

### Debug specific issues

```bash
# Connection issues
--debug=connect,bind

# Permission issues
--debug=own,acl

# Character encoding issues
--debug=iconv

# Performance issues
--debug=io,deltasum,hash
```

## Compatibility with Upstream rsync

The implementation aims for full compatibility with rsync 3.4.1:

- ✅ `-v`, `-vv`, `-vvv` levels match upstream behavior
- ✅ `--info` flags match upstream names and levels
- ✅ `--debug` flags match upstream names and levels
- ✅ Output format matches upstream (except where improved)
- ✅ `--info=help` and `--debug=help` list available flags
- ✅ Flag combinations work as expected

Differences:
- Rust implementation may provide additional tracing in some areas
- Performance characteristics differ (Rust is often faster)
- Some low-level internals differ but behavior is the same

## Examples

### Basic file transfer with progress
```bash
oc-rsync -v --info=progress2 src/ dest/
```

### Debugging a sync issue
```bash
oc-rsync -vvv --debug=flist,filter,del src/ dest/
```

### Performance profiling
```bash
oc-rsync --info=stats3 --debug=io2,deltasum2 src/ dest/
```

### Monitoring a long transfer
```bash
oc-rsync -v --info=progress2,stats src/ remote:/dest/
```

### Maximum debugging
```bash
oc-rsync -vvvvv --debug=all src/ dest/ 2>debug.log
```
