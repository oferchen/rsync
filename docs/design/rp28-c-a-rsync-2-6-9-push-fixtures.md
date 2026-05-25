# RP28.c.a - rsync 2.6.9 Push Interop Fixture Set

Design-only document. No code changes. Specifies the fixture matrix that the
RP28.c push-interop CI cell must exercise. RP28.c.b will land the generator
script and wire the fixtures into the workflow.

## 1. Scope

RP28.c.a specifies the fixtures that the rsync 2.6.9 push-interop CI cell must
exercise. The cell validates oc-rsync receiver behaviour when an upstream
rsync 2.6.9 sender (protocol 28) pushes data through the `rsync://` daemon
transport. RP28.c.b will wire these fixtures into the actual workflow YAML and
land the generator script.

The cell sits inside the RP28 series:

- Parent: RP28 (#2725) - rsync protocol 2.x interop validation umbrella.
- Sibling RP28.b.1 (#2960, merged PR #4903) - source-build helper
  `scripts/build_rsync_2_6_9.sh`.
- Pending RP28.b.2 / RP28.b.3 - wire the build into the CI container and
  cache the artifact.
- Sibling RP28.c (#2728, in-progress) - the workflow cell that this fixture
  spec feeds.
- Sibling RP28.d (#2729, completed) - matching pull cell already shipped.

Cross-reference: see `docs/design/rp28-a-pre30-code-paths-inventory.md` for the
underlying inventory of `protocol_version < 30` code paths these fixtures must
exercise. Memory note: `[[project_protocol_compat]]`.

## 2. Protocol-28 vs Current-Path Differences

The wire-byte regression suite under `crates/protocol/tests/` pins the
following protocol-28-specific divergences that the fixture set must drive
through the receiver pipeline.

### 2.1 flist flags encoding (RP28.g)

- Test: `crates/protocol/tests/flist_wire_flags_rp28g.rs`.
- Protocol 28-29 emits a single-byte flag prefix per file-list entry.
- Protocol 30+ promotes the flag word to varint framing
  (`uses_varint_flist_flags` capability flips at protocol 30 - see
  `crates/protocol/src/version/capabilities.rs:130`).
- The receiver must demote its expectation to the 1-byte form when the
  negotiated peer is protocol 28.
- Upstream reference: `flist.c:send_file_entry()` lines 580-610 in
  `target/interop/upstream-src/rsync-3.4.1/flist.c`.

### 2.2 sort.rs `t_PATH` vs `t_ITEM` (RP28.h)

- Test: `crates/protocol/tests/flist_sort_keys_rp28h.rs`.
- Protocol < 29 sorts the file list by path order (`t_PATH`).
- Protocol >= 29 sorts by item-token order (`t_ITEM`).
- Driving a directory tree wider than the in-list dedup threshold through a
  protocol-28 peer exercises the legacy comparator; ordering divergences
  surface as out-of-order receiver diagnostics or hardlink-pair mismatches.

### 2.3 zlib_codec `see_token` (RP28.i)

- Test: `crates/protocol/tests/zlib_codec_proto_lt_31.rs`.
- Protocol < 31 does NOT advance the deflate dictionary cursor between
  chunks larger than 0xFFFF bytes.
- Protocol >= 31 advances the cursor each iteration (upstream
  `token.c:send_deflated_token()` lines 463-484, conditional on
  `protocol_version >= 31`).
- Inputs that exercise both the < 0xFFFF and > 0xFFFF chunk boundaries against
  a `-z`-compressing protocol-28 sender produce distinct deflate streams; the
  receiver must accept the legacy framing without dictionary drift.

### 2.4 Capability string

- Protocol 28 lacks several modern capability flags:
  - No `i` (INC_RECURSE) - file list must be sent in one shot, not in
    incremental segments.
  - No `C`, `I`, `v`, `u` checksum-negotiation flags - the peer falls back to
    MD4 (protocol 28 default) without XXH3/XXH128/MD5 negotiation.
- `build_capability_string()` in `crates/core/src/setup.rs` is the single
  source of truth for what oc-rsync advertises; the receiver path must accept
  the empty capability set a 2.6.9 sender emits.

## 3. Fixture Matrix

| # | Fixture | What it exercises | Why |
|---|---------|-------------------|-----|
| F1 | Single small file (regular ASCII, ~32 B) | Basic wire format - greeting, flist, single-block delta | Smoke test; first signal of total protocol-28 brokenness. |
| F2 | 100 mixed-size files (1 B - 64 KiB), lowercase-only names | flist ordering at protocol 28 | Exercises the `sort.rs t_PATH` path validated by RP28.h. |
| F3 | 64 KiB file with repeated patterns (long-run RLE-friendly content) | `zlib_codec` at the 0xFFFF chunk boundary | Exercises the `see_token` >0xFFFF gate validated by RP28.i. |
| F4 | Same as F3, transferred with `-z` enabled on the sender | zlib wire format at protocol 28 | Drives the RP28.i regression path end-to-end through compression. |
| F5 | Deep directory tree (5 levels, 4 entries per level = 1365 nodes) | flist directory walk without INC_RECURSE | Exercises the non-INC_RECURSE legacy flist path that a protocol-28 peer always takes. |
| F6 | Mixed file types: 2 symlinks, 1 hardlink pair, 1 FIFO, 1 block-special placeholder | Type-encoding at protocol 28 | Tests the legacy flag-byte encoding for non-regular files (RP28.g surface area). |
| F7 | Filenames with extended UTF-8 characters (e.g. `caf\xc3\xa9.txt`, `\xe6\x97\xa5\xe6\x9c\xac.txt`) | Name encoding pre-iconv | Tests `-8` (`--8-bit-output`) interaction at protocol 28; protocol 28 predates the iconv negotiation that protocol 30+ uses. |
| F8 | 0-byte file (one empty regular file alongside a normal file) | Empty-content framing | Tests the receiver's handling of `len == 0` in the delta loop without tripping a "short read" diagnostic. |
| F9 | Incremental update: run F1 once, modify the single file by appending 1 KiB, push again | quick-check + delta at protocol 28 | Tests delta wire-format compatibility - the second push must produce a single COPY+LITERAL pair, not a full re-send. |
| F10 | Large single file (10 MiB+) with a 4 KiB middle-of-file mutation, transferred twice | Rolling + strong checksum at protocol 28 | Tests the protocol-28 checksum2 wire format (MD4-based, no checksum negotiation). |
| F11 | F5 source tree, then delete two leaf files, push with `--delete` | Receiver-side delete handling at protocol 28 | Tests delete handling under protocol 28; `NDX_DEL_STATS` is protocol >= 31 only (see generator goodbye-phase note), so this fixture verifies the receiver does NOT expect the trailer and does NOT mis-frame the delete IO. |
| F12 | F2 source tree with `--exclude '*.tmp'` and a `--filter '+ keep.dat'` rule | Filter chain over protocol-28 wire | Tests filter encoding interop; protocol 28 transmits the filter rules in the legacy frame shape and the receiver must reproduce the expected post-filter tree. |

All twelve fixtures must yield a destination tree that is byte-identical to
the source side (modulo expected metadata drift the workflow allow-lists -
see section 6).

## 4. Fixture Generation Strategy

Each fixture must be:

- **Deterministic** - any pseudo-random content is seeded from a fixed
  per-fixture constant; re-running the generator on a fresh tmpdir reproduces
  the exact same byte stream. Use `head -c N /dev/zero | tr` style transforms
  or a single Python one-liner with `random.seed(<fixture-id>)`. Do not use
  `/dev/urandom` for content that is asserted against; reserve `/dev/urandom`
  only for fixtures where the assertion is "round-trips byte-for-byte" rather
  than "matches a pre-recorded snapshot".
- **Self-contained** - no network fetch, no host package dependencies beyond
  coreutils + `dd` + `python3`. Everything fits inside a single tmpdir handed
  to the script.
- **Re-runnable** - the script regenerates the source tree identically each
  invocation. It must be safe to invoke twice in the same workflow run (e.g.
  for the F9 incremental case, which runs the generator once, then mutates,
  then re-runs the transfer).

Script location: `scripts/rp28_c_a_fixtures.sh` (RP28.c.b will implement;
RP28.c.a only specifies the contract).

Script CLI contract:

```
scripts/rp28_c_a_fixtures.sh --scale {small|medium|full} --out <tmpdir>
```

- `--scale small` materialises F1-F4 only.
- `--scale medium` materialises F1-F8.
- `--scale full` materialises all twelve fixtures.
- `--out <tmpdir>` is the destination root; the script creates
  `<tmpdir>/f01/`, `<tmpdir>/f02/`, ... and writes a manifest at
  `<tmpdir>/manifest.txt` listing each fixture id, its description, and the
  recommended invocation flags.

## 5. Test Invocation Pattern

For each fixture the workflow step performs:

1. **Source-build rsync 2.6.9** via `scripts/build_rsync_2_6_9.sh` (already
   shipped via RP28.b.1 / PR #4903). The binary is installed at
   `/usr/local/bin/rsync-2.6.9` (or
   `target/interop/upstream-install/2.6.9/bin/rsync` when the existing
   `tools/ci/run_interop.sh build-only` layout is reused).
2. **Build oc-rsync** via the standard `cargo build --release` invocation that
   the surrounding workflow already runs. RP28.c.a does NOT introduce any new
   cargo invocations; CI handles compilation. (Standing rule: agents do not
   run cargo locally.)
3. **Stand up oc-rsync as a receiving daemon**:
   ```
   oc-rsync --daemon --no-detach --config=oc-rsyncd.conf --port=<ephemeral>
   ```
   The config defines a single writable module pointing at the destination
   tmpdir.
4. **Push from the rsync-2.6.9 client**:
   ```
   rsync-2.6.9 -av <fixture-flags> "$SRC/" "rsync://127.0.0.1:<port>/<module>/"
   ```
   The `<fixture-flags>` slot is filled per row in section 3 (e.g. `-z` for
   F4, `--delete` for F11, `--exclude=… --filter=…` for F12).
5. **Diff the destination tree against the expected snapshot**:
   ```
   diff -r "$SRC" "$DST"
   ```
   F9 additionally re-asserts after the second push that only the modified
   file's mtime / content advanced. F11 asserts the two deleted leaves are
   absent from `$DST`.

The cell uses a direct `rsync://` daemon transport, never an `ssh` subprocess
(see section 7).

## 6. Pass / Fail Criteria

A fixture passes when ALL of the following hold:

- **Exit code 0** from the `rsync-2.6.9` client invocation (and 0 from the
  oc-rsync daemon's graceful shutdown).
- **Byte-identical destination tree** - `diff -r "$SRC" "$DST"` produces no
  output. For F11 the comparison runs against the post-delete expected tree
  (source minus the two leaves), not the pre-delete source. For F12 the
  comparison runs against the post-filter expected tree.
- **Clean oc-rsync daemon stderr**:
  - No `panicked at` lines.
  - No `error:` log lines.
  - No `WARNING` lines other than expected protocol-version downgrade
    messages (e.g. `WARNING: client negotiated protocol 28`); the workflow
    allow-lists these by exact prefix.
- **No orphan processes** - `cleanup` trap kills the daemon PID on exit; the
  workflow asserts the port is released before the next fixture runs.

A single fixture failure fails the cell.

## 7. Known Fragilities

The fixture spec and the RP28.c.b workflow must defend against the
following.

- **rsync 2.6.9 is ancient (2009).** The source build needs the compatibility
  shims already in `scripts/build_rsync_2_6_9.sh` (PR #4903 - autotools
  refresh, gcc default-flag overrides). Do not introduce new configure flags
  in RP28.c.b without updating that script.
- **2.6.9 + modern openssh interactions.** The cell MUST use a direct
  `rsync://` daemon transport. Do not invoke 2.6.9 through `ssh`; modern
  openssh defaults reject some of the legacy negotiation modes 2.6.9 still
  emits, producing flaky `Connection reset by peer` failures unrelated to the
  rsync wire protocol.
- **No `--info=stats2` support.** rsync 2.6.9 does not understand
  `--info=stats2`. Any metric extraction must use the simpler `--verbose`
  output (parse `Number of files:`, `Total transferred file size:` etc. from
  the verbose summary).
- **F2 filename-case flakiness.** The sort-order assertion in F2 is sensitive
  to `LC_COLLATE`. Use lowercase-only filenames in the generator and pin
  `LC_ALL=C` in the workflow step to avoid locale-dependent ordering drift
  between glibc and musl runners.
- **2.6.9 hardlink semantics.** Older rsync handled hardlink groups
  differently; F6 must use a single 2-file hardlink pair (not a 3+ file
  group) until RP28.b.2 confirms larger groups round-trip cleanly.
- **`use chroot = false` is required.** The daemon module config must set
  `use chroot = false`; CI runners do not run the workflow as root and
  cannot `chroot()`.
- **Ephemeral port collisions.** Allocate the daemon port via
  `python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'`
  (the pattern already used by the existing RP28.c step). Do not hard-code a
  port; concurrent CI jobs collide.

## 8. RP28.c.b Implementation Pointer

RP28.c.b is the implementation follow-up. Concretely it must land:

- **Fixture generator** at `scripts/rp28_c_a_fixtures.sh` with the CLI
  contract from section 4:
  - `--scale small` (F1-F4) for fast smoke runs.
  - `--scale medium` (F1-F8) for PR-triggered runs.
  - `--scale full` (F1-F12) for nightly / release runs.
  - `--out <tmpdir>` materialisation root.
- **Workflow wiring** in `.github/workflows/_interop.yml` - the file that
  already hosts the existing RP28.c push step (lines 95-187) and the RP28.d
  pull step (lines 189+). RP28.c.b extends the existing
  `Run rsync 2.6.9 push interop (RP28.c)` step (or splits it into a dedicated
  job, e.g. `rp28_2_6_9_push_interop`) so it iterates over the fixture
  matrix instead of the current hard-coded `hello.txt` smoke tree.
- **Runner**: `ubuntu-latest`.
- **Dependency**: the rsync-2.6.9 build artifact cache delivered by RP28.b.3.
  Until RP28.b.3 lands, the cell may keep its current `continue-on-error:
  true` posture and gracefully skip when the binary is absent at
  `target/interop/upstream-install/2.6.9/bin/rsync`.
- **Scale selection**:
  - PR-triggered runs use `--scale medium`.
  - Nightly / `workflow_dispatch` runs use `--scale full`.
  - Selection is controlled by a workflow env var the step reads, defaulting
    to `medium` for unattended runs and `full` for the scheduled nightly.

## 9. Cross-References

- RP28.b.1 build script (PR #4903 - merged): `scripts/build_rsync_2_6_9.sh`.
- RP28.g wire-byte regression: `crates/protocol/tests/flist_wire_flags_rp28g.rs`.
- RP28.h wire-byte regression: `crates/protocol/tests/flist_sort_keys_rp28h.rs`.
- RP28.i wire-byte regression: `crates/protocol/tests/zlib_codec_proto_lt_31.rs`.
- RP28.a inventory: `docs/design/rp28-a-pre30-code-paths-inventory.md`.
- Existing RP28.c smoke step: `.github/workflows/_interop.yml` lines 95-187
  (`Run rsync 2.6.9 push interop (RP28.c)`).
- Existing RP28.d pull cell (sibling): `.github/workflows/_interop.yml` lines
  189+ (`Run rsync 2.6.9 pull interop (RP28.d)`).
- Capability string source of truth: `crates/core/src/setup.rs`
  (`build_capability_string`).
- Protocol-floor constant: `crates/protocol/src/version/constants.rs:7`
  (`OLDEST_SUPPORTED_PROTOCOL = 28`).
- Memory note: `[[project_protocol_compat]]`.
