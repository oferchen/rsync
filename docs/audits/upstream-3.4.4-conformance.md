# Upstream rsync 3.4.4 conformance attestation

Date: 2026-06-10
Scope: 6 oc-rsync UTS-series fixes vs the corresponding upstream rsync 3.4.4 fixes.
Sources: `target/interop/upstream-src/rsync-3.4.4/` + GitHub issues #951, #829, #910, #897, #915, daemon-upload delete-stats.

## Summary

| URV | UTS PR | Upstream issue | Verdict | Follow-ups |
|---|---|---|---|---|
| URV-1 | #5535 (UTS-18) | #951 zlib obuf overflow | DIVERGENT-SAFE | - |
| URV-2 | #5531 (UTS-8) | #829 daemon arg backslash | DIVERGENT-SAFE | #3615 |
| URV-3 | #5523 (UTS-19) | #910 mode-0 sentinel | PARITY | - |
| URV-4 | #5522 + #5532 (UTS-12) | #897 path=/ sender | DIVERGENT-SAFE | #3620 |
| URV-5 | #5525 (UTS-2) | #915 alt-basis daemon | DIVERGENT-RISK | #3617 |
| URV-6 | #5536 (UTS-6) | daemon-upload delete-stats | DIVERGENT-RISK | #3618, #3619 |

Two PARITY/SAFE, three DIVERGENT-SAFE, two DIVERGENT-RISK. The two -RISK verdicts have follow-up tasks filed.

## URV-1: zlib obuf drain (PR #5535 vs #951)

`avail_out_size` formula matches upstream `n*1001/1000+16` exactly (`token.c:344` <-> `zlib_codec.rs:~55`). oc-rsync allocates `SEE_TOKEN_OBUF_SIZE + 16` slack vs upstream `AVAIL_OUT_SIZE(CHUNK_SIZE)` - strictly larger reserve. Drain semantics equivalent through `flate2` state carryover (oc-rsync's input-driven loop vs upstream's `avail_in != 0 || avail_out == 0`). `Z_SYNC_FLUSH` carryover handled identically; `Z_INSERT_ONLY` defaults to `Z_SYNC_FLUSH` in both. No wire impact.

## URV-2: daemon arg backslash (PR #5531 vs #829)

Different root bugs at different layers. Upstream #829: client escapes wildcards, daemon doesn't un-escape. oc-rsync's bug: client never forwarded `--usermap`/`--groupmap` at all. Both manifest the same way (wildcards don't reach receiver). Under `protect_args` (production default), both ship literal byte sequences. Follow-up #3615 tracks the non-protect_args daemon `unbackslash_arg()` parity for full upstream 3.4.4 conformance.

## URV-3: mode-0 sentinel (PR #5523 vs #910)

Identical wire format (regular `FileEntry` with mode=0). Upstream's fix was a carve-out for `mode==0 && missing_args==2` in `flist.c:recv_file_entry` (a rejection upstream introduced in 3.4.3). oc-rsync never had that rejection - the read path stores mode as-is - so the wire-acceptance side was never broken. PR #5523 adds the deletion consumer mirroring upstream's `generator.c:1360-1364`. Behaviorally equivalent.

## URV-4: path = / sender (PRs #5522 + #5532 vs #897)

Architecture differs. Upstream #897 fix in `sender.c:362-381` strips leading slashes from joined `secure_path` before `secure_relative_open(module_dir, relp, ...)`. oc-rsync doesn't use per-file `secure_relative_open`; the daemon passes `module.path` to `ServerConfig` and the engine reads via canonical paths from the flist. The upstream bug class doesn't reproduce. Parser/validator parity is complete (#5522). Follow-up #3620 closes the test-coverage gap.

## URV-5: relative alt-basis daemon (PR #5525 vs #915) - RISK

PR #5525 achieves the SYMLINK-handling parity (oc-rsync's `try_reference_dest` now dispatches on `copy_links` matching upstream `link_stat(..., 0)`). But the RELATIVE alt-basis re-anchor diverges: oc-rsync does `module_path.join(rel)` in `client_args.rs:254-259` with NO kernel-level confinement and NO `..`-rejection fallback. Upstream's `secure_relative_open()` (`syscall.c:1791-1896`) uses `openat2(RESOLVE_BENEATH)` on Linux / `O_RESOLVE_BENEATH` on FreeBSD13+/macOS15+, with a portable fallback rejecting `..` outright.

**Security gap**: `--copy-dest=../../etc` is accepted on a chroot-disabled module today.

Follow-up #3617 (high priority): wire `DirSandbox::open_root(module_path)` (or equivalent lexical guard) around `ref_dir.path` admission.

### URV-5.b.REOPEN update: Landlock allowlist completion

The original URV-5.b.1 follow-up (PR #5568) added wire-layer containment for client-supplied `--temp-dir` / `--partial-dir` / `--backup-dir` paths. That closed the rejection half but left the allowlist half open: `engage_landlock_sandbox` still passed only `module.path` to `restrict_to_module_paths`, so a default-on Landlock flip (URV-5.c.5) would have EACCESed the very in-module paths the operator's configuration permits. The fix:

- Extends `validate_client_paths_in_module` to also cover `--compare-dest` / `--copy-dest` / `--link-dest` (ref_dirs).
- Returns the validated, canonicalised, in-module paths alongside the accept/reject signal.
- Widens `engage_landlock_sandbox` to accept an `extra_allowed_paths: &[&Path]` slice and admits each path to the kernel ruleset as a `PathBeneath` rule.
- Trust boundary unchanged: only paths that already passed the containment check reach the allowlist. The classifier helper `classify_client_path_against_module` is unit-tested for relative-skip, in-module-admit, and out-of-module-reject branches; the fast_io Landlock module gains multi-root admit / multi-root-outside-deny regression coverage.

Regression coverage: `classify_client_path_*` and `allows_writes_under_every_root_in_multi_root_allowlist` / `multi_root_allowlist_still_blocks_paths_outside_every_root` (Linux + landlock feature).

This closes URV-5.b.REOPEN and unblocks URV-5.c.5 (Landlock default-on flip).

## URV-6: NDX_DEL_STATS daemon-upload (PR #5536 vs upstream change) - RISK

Wire format, varint order, and direction all parity. **Verbosity gate diverges**: upstream 3.4.4's whole point in `generator.c:2393, 2437` was REMOVING the `INFO_GTE(STATS, 2)` (`--stats`) requirement - gate is now `delete_mode || force_delete || read_batch`. oc-rsync's `should_send_del_stats()` still requires `do_stats`, so a daemon-upload without `--stats` won't emit, contradicting the 3.4.4 intent.

Minor: protocol gate uses `supports_extended_goodbye()` which is proto 32; upstream uses `protocol_version >= 31`.

Architecture: `run_pipelined_incremental` skips `delete_extraneous_files` entirely, so even with the gate fix, the incremental driver doesn't populate `pending_del_stats`.

Follow-ups: #3618 (gate change + proto-31 check), #3619 (wire delete into incremental driver).

### URV-6.b update: upload-direction gap fixed

The original URV-6.b follow-up only wired `delete_extraneous_files` into
`run_pipelined_incremental`. That closed half the gap (the on-disk deletion)
but not the wire-stats half: the receiver's `handle_goodbye` only emitted
`NDX_DONE`, never `NDX_DEL_STATS`, so the client sender always reported
"Number of deleted files: 0" on a daemon upload regardless of how many
entries the receiver actually swept. Discovered via `runtests.py` while
mirroring the upstream daemon-delete-stats harness. The fix:

- Carries the per-type counters in `ReceiverContext::pending_del_stats`,
  populated by both `run_pipelined` and `run_pipelined_incremental`.
- Emits `NDX_DEL_STATS` from the receiver's `handle_goodbye` whenever
  `flags.delete` is set, matching upstream's
  `delete_mode || force_delete || read_batch` early-emission gate
  (`force_delete` / `read_batch` not yet wired into `ParsedServerFlags`).
- Surfaces the counter into `ClientSummary::items_deleted()` for both
  pull (local receiver) and upload (remote generator stats) so the
  `--stats` renderer reports the correct count regardless of direction.

Regression coverage: `daemon_delete_push_reports_delete_stats` (upload)
and `daemon_delete_pull_reports_delete_stats` (pull) under
`crates/daemon/src/tests/chunks/`.

## Open follow-ups, by priority

| Priority | Task | Title |
|---|---|---|
| High | #3617 | URV-5.a Wire DirSandbox/RESOLVE_BENEATH around daemon relative alt-basis admission (security) |
| High | #3618 | URV-6.a Remove --stats gate on NDX_DEL_STATS emission (match upstream 3.4.4 gates) |
| Medium | #3619 | URV-6.b Wire delete_extraneous_files into run_pipelined_incremental |
| Low | #3615 | URV-2.a Implement upstream 3.4.4 unbackslash_arg() non-protect_args daemon path |
| Low | #3620 | URV-4.a Add e2e test pulling a file from path = / module |

## Provenance

Audit run 2026-06-10 against upstream tarball `rsync-3.4.4.tar.gz` SHA fetched from `download.samba.org/pub/rsync/src/`. Six independent agents, each cross-referencing one UTS PR diff against one upstream issue resolution. Verdicts coded PARITY (byte-byte identical behavior) / DIVERGENT-SAFE (different implementation, same observable behavior on production paths) / DIVERGENT-RISK (semantic divergence with follow-up).
