## Summary

For a local copy, a file skipped purely because of `--max-size` / `--min-size`
emitted its skip notice **after** the final statistics block instead of inline
during the transfer. Upstream `recv_generator()` prints these notices via
`rprintf(FINFO, ...)` in the generator phase and then `goto cleanup`, so they
land **before** `output_summary()` writes the stats block:

```
generator.c:1704   if (max_size >= 0 && F_LENGTH(file) > max_size) {
generator.c:1705       if (INFO_GTE(SKIP, 1)) {
generator.c:1708           rprintf(FINFO, "%s is over max-size\n", fname);
generator.c:1710       goto cleanup;
generator.c:1712   if (min_size >= 0 && F_LENGTH(file) < min_size) {
generator.c:1716           rprintf(FINFO, "%s is under min-size\n", fname);
generator.c:1718       goto cleanup;
```

The local-copy executor emitted the notice with `info_log!`, whose diagnostic
pipeline drains **after** the summary, so the line surfaced late and out of
file order. A drop-in tool parsing the client stream in order depends on the
upstream sequence.

This completes the local-copy skip-notice fidelity work started in #6639, which
routed the other generator-phase notices (`exists`, `not creating new`,
`is newer`) through the core `ClientEvent` path but left the pure size-skip on
the late `info_log!` path because no matching event kind existed.

## Change

- Add `LocalCopyAction::SkippedOverMaxSize` / `SkippedUnderMinSize` and the
  mirroring `ClientEventKind::SkippedOverMaxSize` / `SkippedUnderMinSize`.
- Record the size skip as a client event in the local-copy executor instead of
  emitting `info_log!`, so it renders inline in file order during the generator
  phase, ahead of the stats block. The executor still returns `Ok(false)` so
  `--prune-empty-dirs` accounting continues to treat the file as filtered out.
- Bucket the two notices with the other generator-phase skips in the stable
  partition sort, and render them with upstream-exact text
  (`"%s is over max-size"` / `"%s is under min-size"`) gated on
  `INFO_GTE(SKIP, 1)` (i.e. `-vv`), in both the verbose (`-v`) and
  itemize (`-i`/`--out-format`) renderers.

No wire-protocol change: this is local-copy client output only.

## Tests

- `size_skip_notices_cluster_before_transferred_at_vv` - the two notices surface
  before transferred names, in flist order, with exact text.
- `size_skip_notices_suppressed_below_skip_verbosity` - silent at plain `-v`.
- `size_skips_and_6639_notices_share_generator_phase_bucket` - mixing the size
  skips with the #6639 `exists` / `is newer` notices keeps every generator-phase
  notice ahead of the transferred names (no #6639 regression).
- `max_size_skip_notice_precedes_stats_block` /
  `min_size_skip_notice_precedes_stats_block` - end-to-end binary runs assert the
  notice appears before `Number of files:` in captured stdout.
- `size_skip_notices_suppressed_at_plain_v` - end-to-end silence at `-v`.
