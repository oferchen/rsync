# Incremental File List Integration Design

**Date:** 2026-02-03
**Status:** Approved
**Author:** Claude (with user review)

## Summary

Integrate incremental file list processing into the rsync receiver. Stream file entries as they arrive rather than waiting for the complete list, reducing startup latency for large directory transfers.

## Goals

- Start transfers as soon as first file entries arrive
- Reduce memory pressure by not buffering entire list before processing
- Provide immediate user feedback on progress
- Maintain protocol compatibility with upstream rsync

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Processing model | Interleaved | Maximum parallelism, lowest latency |
| NDX assignment | Arrival order | Protocol-compatible, matches upstream |
| Pipeline filling | Adaptive | Send requests as files become ready |
| Directory failures | Skip subtree | Resilient, continues with unaffected files |

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     Streaming Transfer Loop                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐    ┌───────────────────┐    ┌──────────────┐ │
│  │ Wire Reader  │───▶│ IncrementalFileList│───▶│ Ready Queue  │ │
│  │ (FileEntry)  │    │ (dependency track) │    │ (dirs+files) │ │
│  └──────────────┘    └───────────────────┘    └──────────────┘ │
│                                                      │          │
│                              ┌───────────────────────┘          │
│                              ▼                                  │
│                      ┌──────────────┐                           │
│                      │ Entry Router │                           │
│                      └──────┬───────┘                           │
│                             │                                   │
│              ┌──────────────┼──────────────┐                   │
│              ▼              ▼              ▼                   │
│       ┌──────────┐   ┌──────────┐   ┌──────────┐              │
│       │ Dir      │   │ File     │   │ Symlink/ │              │
│       │ Creator  │   │ Pipeline │   │ Special  │              │
│       └──────────┘   └──────────┘   └──────────┘              │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Main Loop State Machine

The streaming loop manages four concurrent activities with priority-based execution:

```rust
loop {
    // Priority 1: Process ready entries (dirs and files)
    while let Some(entry) = incremental.pop() {
        if entry.is_dir() {
            create_directory_or_mark_failed(&entry, &mut failed_dirs);
        } else if entry.is_file() {
            if !is_under_failed_dir(&entry, &failed_dirs) {
                files_ready.push_back((next_ndx, entry));
                next_ndx += 1;
            }
        }
    }

    // Priority 2: Fill pipeline with file requests
    while pipeline.can_send() && !files_ready.is_empty() {
        let (ndx, entry) = files_ready.pop_front();
        send_file_request(ndx, &entry, ...);
        pipeline.push(pending);
    }

    // Priority 3: Read more entries from wire (if not at EOF)
    if !flist_reader.is_finished() {
        if let Some(entry) = flist_reader.try_read_entry()? {
            incremental.push(entry);
            continue;
        }
    }

    // Priority 4: Process one response (if pipeline non-empty)
    if !pipeline.is_empty() {
        let pending = pipeline.pop();
        process_file_response(pending)?;
        continue;
    }

    // Exit condition
    if flist_reader.is_finished() && pipeline.is_empty() {
        break;
    }
}
```

## Failed Directory Tracking

```rust
/// Tracks directories that failed to create.
struct FailedDirectories {
    paths: HashSet<String>,
}

impl FailedDirectories {
    fn mark_failed(&mut self, path: &str) {
        self.paths.insert(path.to_string());
    }

    fn failed_ancestor(&self, entry_path: &str) -> Option<&str> {
        let mut check_path = entry_path;
        while let Some(pos) = check_path.rfind('/') {
            check_path = &check_path[..pos];
            if self.paths.contains(check_path) {
                return Some(check_path);
            }
        }
        None
    }
}
```

## New API Methods

Add to `IncrementalFileListReceiver`:

```rust
/// Non-blocking attempt to read and process one entry.
pub fn try_read_one(&mut self) -> io::Result<bool>

/// Returns iterator over currently ready entries.
pub fn drain_ready(&mut self) -> impl Iterator<Item = FileEntry>
```

## Extended Statistics

```rust
pub struct TransferStats {
    // Existing
    pub files_transferred: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,

    // New for incremental mode
    pub entries_received: u64,
    pub directories_created: u64,
    pub directories_failed: u64,
    pub files_skipped: u64,
    pub symlinks_created: u64,
    pub specials_created: u64,
}
```

## Error Handling

| Case | Handling |
|------|----------|
| Empty file list | Complete normally |
| All directories failed | Warn, complete with zero transfers |
| Orphaned entries | Report at end, count as skipped |
| Wire read error | Mark finished, drain pipeline, report partial |
| Pipeline stall | Force read entry if pipeline can send but files_ready empty |

## Implementation Tasks

| # | Task | File | Lines | Complexity |
|---|------|------|-------|------------|
| 1 | Add `FailedDirectories` struct | `receiver.rs` | ~40 | Low |
| 2 | Add `try_read_one()` method | `receiver.rs` | ~20 | Low |
| 3 | Add `process_ready_entry()` helper | `receiver.rs` | ~60 | Medium |
| 4 | Refactor `run_pipelined()` main loop | `receiver.rs` | ~150 | High |
| 5 | Extend `TransferStats` | `lib.rs` | ~20 | Low |
| 6 | Update finalization for orphans | `receiver.rs` | ~30 | Low |
| 7 | Unit tests | `receiver.rs` | ~200 | Medium |
| 8 | Integration tests | `tests/` | ~100 | Medium |

**Total: ~620 lines**

## Implementation Order

```
1. FailedDirectories struct
       ↓
2. try_read_one() method
       ↓
3. process_ready_entry() helper
       ↓
4. Refactor run_pipelined()
       ↓
5. TransferStats extensions
       ↓
6. Orphan handling
       ↓
7. Unit tests
       ↓
8. Integration tests
```

## Testing Strategy

**Unit Tests:**
- Flat directory (all immediately ready)
- Nested directories in order
- Nested directories out of order
- Failed directory skips children
- Empty file list
- Orphaned entries
- Mixed entry types

**Integration Tests:**
- Mock sender with wire format
- Upstream rsync interop (if available)

**Property Tests:**
- All entries eventually processed
- Parents always before children

## Rollout Plan

1. Implement behind feature flag `incremental_flist` (default off)
2. Test extensively against upstream rsync
3. Enable by default after validation
4. Keep `run_sync()` as fallback

## Dependencies

Existing infrastructure (already implemented):
- `protocol::flist::IncrementalFileList` - dependency tracking
- `protocol::flist::IncrementalFileListBuilder` - configuration
- `IncrementalFileListReceiver` - wire reader wrapper (in receiver.rs)

## Performance Expectations

| Metric | Batch Mode | Incremental Mode |
|--------|------------|------------------|
| Time to first transfer (10k files) | ~200ms | ~2ms |
| Memory peak | O(n) entries | O(pipeline_window) |
| Total transfer time | Baseline | Same or slightly better |
