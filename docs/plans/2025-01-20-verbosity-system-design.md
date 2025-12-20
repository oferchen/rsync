# Verbosity System Design

## Overview

Implement complete `--info` and `--debug` flag support matching upstream rsync 3.4.1, with proper `-v`/`-vv`/`-vvv` level mapping.

## Design Decisions

1. **Structured logging config** - Rust-idiomatic `VerbosityConfig` struct with typed fields
2. **Lazy macro-based logging** - `info_log!`, `debug_log!` macros with zero cost when disabled
3. **Thread-local storage** - Config set once at session start, accessed via macros
4. **Event-based output** - CLI renders events, supports `--msgs2stderr`
5. **Upstream-compatible mapping** - Same flags and verbose level mapping as rsync 3.4.1

## Info Flags (13 total)

| Flag | Levels | Description |
|------|--------|-------------|
| BACKUP | 1 | Mention files backed up |
| COPY | 1 | Mention files copied locally |
| DEL | 1 | Mention deletions on receiving side |
| FLIST | 1-2 | File-list receiving/sending |
| MISC | 1-2 | Miscellaneous information |
| MOUNT | 1 | Mounts found or skipped |
| NAME | 1-2 | Updated names (1), unchanged (2) |
| NONREG | 1 | Skipped non-regular files |
| PROGRESS | 1-2 | Per-file (1), overall (2) |
| REMOVE | 1 | Files removed on sending side |
| SKIP | 1-2 | Files skipped due to overrides |
| STATS | 1-3 | Statistics at end of run |
| SYMSAFE | 1 | Unsafe symlinks |

## Debug Flags (24 total)

| Flag | Levels | Description |
|------|--------|-------------|
| ACL | 1 | Debug extra ACL info |
| BACKUP | 1-2 | Debug backup actions |
| BIND | 1 | Debug socket bind actions |
| CHDIR | 1 | Debug directory changes |
| CONNECT | 1-2 | Debug connection events |
| CMD | 1-2 | Debug commands/options issued |
| DEL | 1-3 | Debug delete actions |
| DELTASUM | 1-4 | Debug delta-transfer checksumming |
| DUP | 1 | Debug weeding of duplicate names |
| EXIT | 1-3 | Debug exit events |
| FILTER | 1-3 | Debug filter actions |
| FLIST | 1-4 | Debug file-list operations |
| FUZZY | 1-2 | Debug fuzzy scoring |
| GENR | 1 | Debug generator functions |
| HASH | 1 | Debug hashtable code |
| HLINK | 1-3 | Debug hard-link actions |
| ICONV | 1-2 | Debug iconv conversions |
| IO | 1-4 | Debug I/O routines |
| NSTR | 1 | Debug negotiation strings |
| OWN | 1-2 | Debug ownership changes |
| PROTO | 1 | Debug protocol information |
| RECV | 1 | Debug receiver functions |
| SEND | 1 | Debug sender functions |
| TIME | 1-2 | Debug setting of modified times |

## Verbose Level Mapping (from upstream options.c)

```
Level 0: info=NONREG
Level 1: info=COPY,DEL,FLIST,MISC,NAME,STATS,SYMSAFE
Level 2: info=BACKUP,MISC2,MOUNT,NAME2,REMOVE,SKIP
         debug=BIND,CMD,CONNECT,DEL,DELTASUM,DUP,FILTER,FLIST,ICONV
Level 3: debug=ACL,BACKUP,CONNECT2,DELTASUM2,DEL2,EXIT,FILTER2,FLIST2,FUZZY,GENR,OWN,RECV,SEND,TIME
Level 4: debug=CMD2,DELTASUM3,DEL3,EXIT2,FLIST3,ICONV2,OWN2,PROTO,TIME2
Level 5: debug=CHDIR,DELTASUM4,FLIST4,FUZZY2,HASH,HLINK
```

## Files to Create/Modify

### New Files
- `crates/logging/src/verbosity.rs` - VerbosityConfig, InfoLevels, DebugLevels structs
- `crates/logging/src/verbosity/thread_local.rs` - Thread-local storage and init
- `crates/logging/src/verbosity/macros.rs` - info_log!, debug_log! macros
- `crates/logging/src/events.rs` - InfoEvent, DebugEvent types

### Modified Files
- `crates/logging/src/lib.rs` - Export new modules
- `crates/cli/src/frontend/execution/flags.rs` - Expand flag parsing
- `crates/cli/src/frontend/progress/render.rs` - Render diagnostic events

## Usage Examples

```rust
// Initialize at session start
logging::verbosity::init(VerbosityConfig::from_verbose_level(2));

// Use anywhere in codebase
info_log!(Name, 1, "{}", file_path);
debug_log!(Deltasum, 2, "block {} checksum mismatch", block_num);
debug_log!(Filter, 1, "excluding {:?} due to pattern {:?}", path, pattern);
```

## Testing Strategy

1. Unit tests for flag parsing (all 37 flags + negation + levels)
2. Unit tests for verbose level mapping
3. Integration tests comparing output with upstream rsync
4. Snapshot tests for help text format
