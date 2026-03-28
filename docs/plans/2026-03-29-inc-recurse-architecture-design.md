# INC_RECURSE Sender Architecture Design

Date: 2026-03-29

## Problem

The current sender-side INC_RECURSE implementation has SOLID violations:

1. **SRP violation** - `send_extra_file_lists()` is an 80-line method doing 5 things: guard checks, writer management, segment iteration, wire encoding, and EOF sending.
2. **Temporal coupling** - Sub-lists are sent eagerly before the transfer loop (step 6), preventing lazy interleaving that upstream uses.
3. **Inconsistent lookup** - `wire_to_flat_ndx()` uses linear reverse scan while `flat_to_wire_ndx()` uses `partition_point`.

## Chosen Approach: Hybrid (Eager Scan, Lazy Sending)

- **Eager filesystem scan**: Leverage parallel stat. Build the complete file list upfront.
- **Lazy sub-list sending**: Interleave sub-list emission with the transfer loop, matching upstream `sender.c:227,261` cadence using `MIN_FILECNT_LOOKAHEAD = 1000`.

## Architecture: 3-Component Decomposition

```
Partitioner (inc_recurse.rs) --- classifies + reorders ---> Vec<PendingSegment>
                                                                    |
SegmentScheduler --- yields segments on demand ---------------------+
        |
        v
encode_and_send_segment() --- writes wire format ---> multiplexed I/O
```

### Component 1: Partitioner (existing, already optimized)

File: `crates/transfer/src/generator/file_list/inc_recurse.rs`

No changes needed. Already optimized with:
- 1 HashMap + 2 dense Vecs (down from 4 HashMaps)
- TaggedIndex for O(1) node_id propagation
- Move semantics via `Option<T>::take()` instead of clone

### Component 2: SegmentScheduler

Cursor-based iterator over `Vec<PendingSegment>` with lookahead throttling.

```rust
struct SegmentScheduler {
    segments: Vec<PendingSegment>,
    cursor: usize,
}
```

Interface:
- `new(segments)` - takes ownership of pending segments
- `next_if_needed(remaining_in_current) -> Option<&PendingSegment>` - yields next segment when remaining files drop below `MIN_FILECNT_LOOKAHEAD`
- `drain_remaining() -> slice` - flushes all remaining segments (for post-loop cleanup)
- `is_exhausted() -> bool` - true when all segments consumed

### Component 3: Segment Encoding (method on GeneratorContext)

Extracted from the current god method. Encodes a single segment to the wire:

1. Compute ndx_start from previous segment boundary
2. Write `NDX_FLIST_OFFSET - parent_dir_ndx`
3. Set `first_ndx` on flist_writer
4. Write file entries
5. Write end-of-list marker
6. Update ndx_segments table

### Transfer Loop Integration

Move sub-list sending from step 6 (before loop) INTO the loop body:

```
build_file_list -> partition -> send_initial_list -> send_id_lists -> send_io_error_flag
    -> run_transfer_loop:
        top-of-loop:  scheduler.next_if_needed() -> encode_and_send_segment()
        ... normal transfer (read NDX, send delta) ...
        bottom-of-loop: scheduler.next_if_needed() -> encode_and_send_segment()
    -> flush remaining segments + send NDX_FLIST_EOF
    -> handle_goodbye
```

### Additional Fix: wire_to_flat_ndx

Replace linear reverse scan with `partition_point` to match `flat_to_wire_ndx`.

## Testing Strategy

| Component | Test Type | Verification |
|-----------|-----------|-------------|
| Partitioner | Unit | dir_ndx assignment, depth-first order, move semantics |
| SegmentScheduler | Unit | Lookahead throttling, drain, empty input |
| Segment encoding | Golden byte | Wire output matches upstream byte-for-byte |
| Integration | Interop | End-to-end with upstream rsync 3.4.1 |
| wire_to_flat_ndx | Unit | Binary search correctness |
