# Rsync Tracing Quick Reference

## Command Line Flags

### Verbosity Levels

```bash
# No flags - minimal output
oc-rsync src/ dest/

# -v (level 1) - basic file list
oc-rsync -v src/ dest/

# -vv (level 2) - detailed info + basic debug
oc-rsync -vv src/ dest/

# -vvv (level 3) - protocol debugging
oc-rsync -vvv src/ dest/

# -vvvv (level 4) - extensive debug
oc-rsync -vvvv src/ dest/

# -vvvvv (level 5+) - maximum verbosity
oc-rsync -vvvvv src/ dest/
```

### Info Flags (`--info=FLAGS`)

```bash
# Show all available flags
oc-rsync --info=help

# Common flags
--info=progress2     # Overall progress indicator
--info=stats         # Transfer statistics
--info=name2         # Show all files (not just changed)
--info=del           # Show deletions
--info=copy          # Show file copies

# Disable flags
--info=nocopy        # Don't show copies
--info=name0         # Don't show names

# Combine flags
--info=progress2,stats,del

# Special values
--info=all           # Enable all info flags
--info=none          # Disable all info output
```

### Debug Flags (`--debug=FLAGS`)

```bash
# Show all available flags
oc-rsync --debug=help

# Protocol debugging
--debug=proto        # Protocol negotiation
--debug=connect      # Connection establishment

# Transfer debugging
--debug=recv         # Receiver operations
--debug=send         # Sender operations
--debug=deltasum     # Delta computation

# I/O debugging
--debug=io           # I/O operations
--debug=flist        # File list operations

# With levels
--debug=io2          # More detailed I/O
--debug=flist4       # Maximum file list detail

# Combine flags
--debug=proto,recv,send

# Special values
--debug=all          # Enable all debug flags
--debug=none         # Disable all debug output
```

## Common Use Cases

### Monitor Transfer Progress

```bash
oc-rsync -v --info=progress2 large_dir/ backup/
```

### Debug Connection Issues

```bash
oc-rsync -vvv --debug=proto,connect src/ remote:/dest/
```

### Debug Sync Issues

```bash
oc-rsync -vv --debug=flist2,filter src/ dest/
```

### Performance Analysis

```bash
oc-rsync --info=stats3 --debug=io,deltasum src/ dest/
```

### Quiet Mode with Stats

```bash
oc-rsync --quiet --info=stats src/ dest/
```

### Show Only Changes

```bash
oc-rsync -v --info=name0 --itemize-changes src/ dest/
```

## Rust Code Examples

### Using Standard Tracing

```rust
use tracing::{debug, info, trace};

// Info-level events
info!(target: "rsync::copy", "copying {}", path);
info!(target: "rsync::stats", "transferred {} bytes", bytes);

// Debug-level events
debug!(target: "rsync::protocol", "negotiated version {}", version);
debug!(target: "rsync::receiver", "received block offset={}", offset);

// Trace-level events (very detailed)
trace!(target: "rsync::io", "read {} bytes from fd {}", count, fd);
```

### Using Convenience Macros

```rust
use logging::*;

// Same as above but more concise
trace_copy!("copying {}", path);
trace_stats!("transferred {} bytes", bytes);
trace_proto!("negotiated version {}", version);
trace_recv!("received block offset={}", offset);
trace_io!("read {} bytes from fd {}", count, fd);
```

### Initializing Tracing

```rust
use logging::{VerbosityConfig, init_tracing};

// From verbosity level
let config = VerbosityConfig::from_verbose_level(2);
init_tracing(config);

// From specific flags
let mut config = VerbosityConfig::default();
config.apply_info_flag("copy2").unwrap();
config.apply_debug_flag("proto").unwrap();
init_tracing(config);
```

### Checking Verbosity

```rust
use logging::{info_gte, debug_gte, InfoFlag, DebugFlag};

if info_gte(InfoFlag::Copy, 1) {
    // This code runs if copy info is enabled at level 1+
}

if debug_gte(DebugFlag::Proto, 2) {
    // This code runs if protocol debug is enabled at level 2+
}
```

## Environment Variables

```bash
# Standard Rust tracing (works alongside rsync verbosity)
RUST_LOG=debug oc-rsync src/ dest/

# Filter by module
RUST_LOG=rsync::protocol=debug,rsync::io=trace oc-rsync src/ dest/

# Combine with rsync flags
RUST_LOG=debug oc-rsync -vv src/ dest/
```

## Output Format

### Info Messages

```
rsync: copying file.txt
rsync: deleting obsolete.txt
```

### Debug Messages

```
[Receiver] received 4096 bytes
[Proto] negotiated protocol version 31
[Delta] computed 128 blocks for file.dat
```

### Progress

```
file.txt
      1,234,567  50%   123.45kB/s    0:00:08
```

### Statistics

```
Number of files: 123 (reg: 100, dir: 20, link: 3)
Total file size: 1.23M bytes
Literal data: 456K
Matched data: 789K

sent 100.5K bytes  received 89.2K bytes  126.5K bytes/sec
total size is 1.23M  speedup is 6.49
```

## Flag Reference

### Info Flags

| Flag | Levels | Description |
|------|--------|-------------|
| `backup` | 0-∞ | Backup file operations |
| `copy` | 0-∞ | File copy operations |
| `del` | 0-∞ | File deletion operations |
| `flist` | 0-2 | File list building |
| `misc` | 0-2 | Miscellaneous info |
| `mount` | 0-∞ | Mount point warnings |
| `name` | 0-2 | File names (0=off, 1=changed, 2=all) |
| `nonreg` | 0-∞ | Non-regular files |
| `progress` | 0-2 | Progress (0=off, 1=per-file, 2=overall) |
| `remove` | 0-∞ | File removal operations |
| `skip` | 0-2 | Skipped files |
| `stats` | 0-3 | Transfer statistics |
| `symsafe` | 0-∞ | Symlink safety warnings |

### Debug Flags

| Flag | Levels | Description |
|------|--------|-------------|
| `acl` | 0-∞ | ACL processing |
| `backup` | 0-∞ | Backup creation |
| `bind` | 0-∞ | Socket binding |
| `chdir` | 0-∞ | Directory changes |
| `cmd` | 0-2 | Command execution |
| `connect` | 0-2 | Connection establishment |
| `del` | 0-3 | Deletion operations |
| `deltasum` | 0-4 | Delta computation |
| `dup` | 0-∞ | Duplicate detection |
| `exit` | 0-3 | Exit status |
| `filter` | 0-2 | Filter rules |
| `flist` | 0-4 | File list operations |
| `fuzzy` | 0-∞ | Fuzzy matching |
| `genr` | 0-∞ | Generator operations |
| `hash` | 0-∞ | Hash calculations |
| `hlink` | 0-∞ | Hard link detection |
| `iconv` | 0-2 | Character encoding |
| `io` | 0-4 | I/O operations |
| `nstr` | 0-∞ | Name strings |
| `own` | 0-2 | Ownership changes |
| `proto` | 0-2 | Protocol negotiation |
| `recv` | 0-∞ | Receiver operations |
| `send` | 0-∞ | Sender operations |
| `time` | 0-2 | Timing information |

## Tips

1. **Start with `-v`** and add more v's as needed
2. **Use `--info=help`** to see all available info flags
3. **Use `--debug=help`** to see all available debug flags
4. **Combine flags** for targeted debugging
5. **Use `--quiet`** to suppress output except stats/errors
6. **Redirect stderr** for debug logs: `2>debug.log`
7. **Use RUST_LOG** for development/debugging only

## Troubleshooting

### No output with -v

- Check for `--quiet` flag
- Check for `--info=none`
- Verify files are being transferred

### Too much output

- Reduce verbosity level
- Disable specific flags: `--info=nomisc,noskip`
- Use `--quiet --info=stats` for summary only

### Missing debug information

- Ensure debug flags are enabled: `--debug=FLAG`
- Increase level: `--debug=FLAG2` or `--debug=FLAG3`
- Use `-vvv` or higher for automatic debug enablement
