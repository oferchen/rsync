# INC_RECURSE Implementation Analysis

## Overview

The `CF_INC_RECURSE` compatibility flag enables **incremental recursion**, a major optimization in rsync protocol 30+ that changes how file lists are built and transmitted.

## Current Behavior (Non-Incremental)

Our current implementation (and rsync with `--no-inc-recurse`):

1. **Build Phase**: Recursively scan entire source tree
2. **Sort Phase**: Sort all files/directories
3. **Transmit Phase**: Send complete file list as one batch
4. **Process Phase**: Receiver processes entire list

### Code Location
- `crates/core/src/server/generator.rs::build_file_list()` - Builds complete list
- `crates/core/src/server/generator.rs::send_file_list()` - Sends entire list at once
- `crates/core/src/server/receiver.rs::receive_file_list()` - Receives complete list

### Limitations
- High memory usage for large directory trees (all files in memory)
- Delayed start: must scan everything before transfer begins
- Poor progress reporting: can't start transfer until scan completes

## Incremental Recursion Behavior

With `inc_recurse = 1`:

1. **Initial**: Send top-level directories and immediate files
2. **Stream**: Send `NDX_FLIST_EOF` marker to signal segment end
3. **Process**: Receiver/generator start processing immediately
4. **Iterate**: As directories are processed, send their contents
5. **Queue**: Maintain priority queue of pending directories
6. **Complete**: Send final `NDX_FLIST_EOF` when all dirs processed

### Protocol Changes

```
Non-incremental:
  Sender: [all files...] [0x00]
  Receiver: Waits for complete list, then processes

Incremental:
  Sender: [top-level files...] [NDX_FLIST_EOF]
          [dir1 contents...] [NDX_FLIST_EOF]
          [dir2 contents...] [NDX_FLIST_EOF]
          ...
          [final NDX_FLIST_EOF when send_dir_ndx < 0]
  Receiver: Processes each segment as it arrives
```

### Key Upstream Variables

- `inc_recurse` (global): Flag indicating mode is active
- `send_dir_ndx`: Current directory being sent (-1 when done)
- `send_dir_depth`: Current recursion depth
- `dir_flist`: Separate list tracking directories
- `cur_flist`: Current file list segment being built
- `NDX_FLIST_EOF`: Special marker value (defined in rsync.h)

### Upstream Code References

- `flist.c:2192` - `send_file_list()` with incremental logic
- `flist.c:2157` - `add_dirs_to_tree()` manages directory queue
- `flist.c:2540` - Final EOF marker when `send_dir_ndx < 0`
- `flist.c:2766` - `recv_file_list()` handles `NDX_FLIST_EOF`
- `generator.c` - Processes file list segments as they arrive

## Implementation Requirements

### 1. Data Structure Changes

**Generator Side (Sender):**
```rust
pub struct Generator {
    // Current implementation
    file_list: Vec<FileEntry>,  // ❌ Holds ALL files

    // Incremental recursion needs
    cur_flist: Vec<FileEntry>,      // ✅ Current segment
    dir_flist: Vec<FileEntry>,      // ✅ Directory queue
    send_dir_ndx: isize,            // ✅ Current dir (-1 = done)
    send_dir_depth: usize,          // ✅ Recursion depth
    inc_recurse: bool,              // ✅ Mode flag
}
```

**Receiver Side:**
```rust
pub struct Receiver {
    // Must handle multiple file list segments
    file_lists: Vec<Vec<FileEntry>>,  // ✅ Multiple segments
    current_segment: usize,            // ✅ Which segment processing
    flist_eof_received: bool,          // ✅ Final EOF marker
}
```

### 2. Protocol Changes

**Add NDX Constants:**
```rust
// In protocol crate
pub const NDX_FLIST_EOF: i32 = -101;  // Marks end of file list segment
// (Exact value from rsync.h)
```

**File List Wire Format:**
```rust
// Current: send all, then 0x00
fn send_file_list(&self, writer: &mut W) -> io::Result<()> {
    for entry in &self.file_list {
        write_entry(writer, entry)?;
    }
    write_varint(writer, 0)?;  // End marker
}

// Incremental: send segments with NDX_FLIST_EOF between
fn send_file_list_incremental(&mut self, writer: &mut W) -> io::Result<()> {
    loop {
        // Send current directory's contents
        for entry in &self.cur_flist {
            write_entry(writer, entry)?;
        }
        write_varint(writer, NDX_FLIST_EOF)?;  // Segment end

        if self.send_dir_ndx < 0 {
            write_varint(writer, NDX_FLIST_EOF)?;  // Final EOF
            break;
        }

        // Get next directory from queue
        self.advance_to_next_directory()?;
    }
}
```

### 3. Directory Queue Management

```rust
impl Generator {
    fn add_dirs_to_tree(&mut self, parent_ndx: isize) -> io::Result<()> {
        // Add discovered directories to dir_flist
        // Track parent/child relationships
        // Maintain send_dir_ndx for traversal
    }

    fn advance_to_next_directory(&mut self) -> io::Result<()> {
        // Move send_dir_ndx to next directory
        // Depth-first: go to first child, then siblings, then parent's sibling
        // Build cur_flist from directory contents
    }
}
```

### 4. Generator/Receiver Coordination

**Generator must:**
- Send file list segments interleaved with data transfer
- Respond to receiver requests for more directories
- Track which directories have been sent

**Receiver must:**
- Process file list segments as they arrive
- Request next segments when ready
- Handle partial file lists during processing

## Complexity Estimate: 16-24 Hours

### Breakdown

1. **Data Structure Refactoring (4-6 hours)**
   - Split file_list into cur_flist + dir_flist
   - Add directory queue management
   - Update Generator/Receiver constructors

2. **Protocol Implementation (6-8 hours)**
   - Add NDX_FLIST_EOF constant and handling
   - Implement segmented send_file_list
   - Update receive_file_list for segments
   - Add directory traversal logic

3. **Generator/Receiver Integration (4-6 hours)**
   - Coordinate segment transmission with processing
   - Handle interleaved file list and data transfer
   - Update delta transfer loop

4. **Testing (2-4 hours)**
   - Unit tests for directory queue
   - Integration tests for segmented transmission
   - Interop tests with upstream rsync (inc_recurse mode)
   - Large directory tree stress tests

## Alternative: Defer INC_RECURSE

### Arguments for Deferral

1. **Non-Critical**: Works fine without it (slightly higher memory, slower start)
2. **Complex**: Requires significant architectural changes
3. **Alternatives Exist**: Can use `--no-inc-recurse` for large trees
4. **Already Advertised**: Flag is set during negotiation, just not implemented

### Arguments for Implementation

1. **Protocol 30+ Default**: Modern rsync clients expect this
2. **Performance**: Significant improvement for large trees
3. **Completeness**: Required for full protocol 30+ parity
4. **User Expectations**: Listed as "High-Value" compatibility flag

## Recommendation

**Defer INC_RECURSE until after:**
1. Core protocol 30+ features are stable
2. Basic transfer scenarios are fully tested
3. Interop with upstream is validated for non-incremental mode

**Current Status:**
- ✅ Flag is properly advertised during negotiation
- ✅ Non-incremental mode works correctly
- ❌ Incremental protocol not implemented (behaves as non-incremental)

When ready to implement, use this document as the design specification.

## References

- Upstream `flist.c:2192-2560` - `send_file_list()` incremental logic
- Upstream `flist.c:2561-2785` - `recv_file_list()` segment handling
- Upstream `generator.c` - Directory processing coordination
- Upstream `rsync.h` - NDX constant definitions
- Upstream `compat.c:161-178` - `set_allow_inc_recurse()` conditions
