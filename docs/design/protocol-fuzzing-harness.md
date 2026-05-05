# Protocol fuzzing harness against live upstream rsync

Design note for TODO #1193. Sister to the wire-byte fuzz harness in
`crates/protocol/fuzz/` (#1194) and the wire-format differential fuzzer
tracked in #1303. This note covers a **behavioural** differential
harness: drive both oc-rsync and upstream rsync 3.4.1 through equivalent
randomized end-to-end transfer scenarios, then assert behavioural
equivalence (exit code, stderr structure, byte-identical destination
trees, byte-identical wire payload via tcpdump).

## Problem statement

The existing protocol fuzz harness at
`crates/protocol/fuzz/fuzz_targets/` (#1194) is bytes-only:

- `fuzz_varint.rs`, `varint_roundtrip.rs` - `read_varint` / `write_varint`.
- `fuzz_multiplex_frame.rs`, `multiplex_frame.rs` - mux frame parser.
- `fuzz_legacy_greeting.rs` - legacy `@RSYNCD:` greeting parser.
- `fuzz_delta.rs` - delta-token decoder.
- `file_entry_roundtrip.rs` - file-entry encode/decode.

These targets reliably catch malformed-input crashes, panic-on-overflow,
out-of-bounds indexing, and decoder hangs. They cannot catch
**behavioural divergence**: the case where oc-rsync and upstream rsync
both accept the same input as well-formed but produce subtly different
outputs.

Worked example. Suppose oc-rsync handles protocol-30 file-list ordering
in the INC_RECURSE segment slightly differently from upstream (e.g.
ties broken by uid before name vs. name before uid). For N <= ~1000
files the difference never surfaces because the list also fits in a
single segment and the upstream oracle's final ordering matches. At
N > 1000 the difference manifests as a destination-tree divergence that
the bytes-only fuzz harness will never see, because it never runs the
full transfer end-to-end.

The static counterparts in
`crates/protocol/tests/golden_protocol_v28_handshake.rs`,
`golden_protocol_v28_wire.rs`, `golden_protocol_v28_flist.rs`,
`golden_protocol_v28_mplex_delta_stats.rs`, `golden_protocol_v29_flist.rs`,
`golden_protocol_v29_wire.rs`, and `golden_handshakes.rs` lock the wire
format for fixed scenarios, and `tools/ci/run_interop.sh` drives a
fixed matrix against rsync 3.0.9, 3.1.3, 3.4.1. Both are sound but
finite. Neither randomizes inputs; neither finds behavioural
divergences outside their hand-curated corpus.

The harness specified here closes that gap. It is the "interop fuzz"
complement to the static golden-byte tests and the fixed interop
matrix.

## Scope

### Protocol versions

All five revisions enumerated in `crates/protocol/src/version/constants.rs`:

- 28, 29, 30, 31, 32. The handshake matrix sits in
  `crates/protocol/src/version/` (constants.rs:7-11 anchor the bounds,
  `select_highest_mutual` at `select.rs` performs negotiation).
- 30 is the legacy/binary handshake boundary
  (`FIRST_BINARY_NEGOTIATION_PROTOCOL = 30`); both sides of that
  boundary must be exercised.
- The harness pins the version per-iteration with
  `--protocol=<n>` so neither implementation can silently upgrade.

### Transports / modes

- Local copy (single-process, no socket).
- Daemon push (`oc-rsync` -> `rsync --daemon` via `rsync://`).
- Daemon pull (`rsync --daemon` -> `oc-rsync`).
- SSH push (forced through `oc-rsync` with `-e "ssh -o ..."`).
- SSH pull (mirror of above).
- Batch write + replay (`--write-batch` + `--read-batch`, both sides).

The matrix mirrors `tools/ci/run_interop.sh` lines 50-65 (versions and
the daemon-push / daemon-pull permutations) and extends it to local +
SSH + batch.

### Compression

- None.
- zlib (`--compress`).
- zstd (post #1100).
- lz4 (post #1379-#1381).

The codec set is enumerated by the `Compressor` trait
(`crates/compress/`); the harness picks one randomly per iteration and
asserts both sides negotiate to the same codec via the wire log.

### Filter rules

- No filters.
- Exclude-only (`--exclude PATTERN`).
- Include + exclude (`--include`, `--exclude`).
- Per-directory `.rsync-filter` (`--filter=':e .rsync-filter'`).
- Merge files (`--filter='. file'`).

The rule set is bounded by the parser in `crates/filters/`; the harness
generates rules from the same grammar the parser expects.

### Metadata flags

- `-a` (archive: `-rlptgoD`).
- `-A` (POSIX ACLs).
- `-X` (xattrs).
- `--hard-links`.
- `--sparse`.

The flag universe is tagged by which oracle assertions remain meaningful
(see "Equivalence oracles" below). For example, `-A` and `-X` only
assert when both peers are on a filesystem that supports the relevant
metadata.

## Mutation strategies

### Tree mutation

Random directory trees:

- Size: 1 to 10 000 files. Lower bound exercises empty-list edge cases;
  upper bound exercises INC_RECURSE multi-segment paths.
- Depth: 1 to 10 levels. Cap prevents pathological recursion costs.
- Names: ASCII baseline plus unicode + special-char fuzzing per #1528,
  #1529 (path encoding hardening). Generator pulls from a curated
  corpus: NFC/NFD pairs, RTL marks, control bytes (`\x01`-`\x1f`),
  embedded slashes (rejected), embedded null (rejected),
  whitespace-only names, `.` / `..` that must be sanitized.

### Byte mutation

File-content generators, drawn uniformly per file:

- Empty (`size = 0`).
- All-zero (sparse-friendly).
- All-`0xff` (compression-hostile).
- Repeating short pattern (`abcabc...`, periods 2-256).
- Random bytes (incompressible, rsync's worst case).
- Sparse with holes (random hole offsets, fixed total size).

The mix tilts the harness toward all the regimes the delta engine
treats specially.

### Metadata mutation

- Permissions: random `0o000`-`0o777`, occasional setuid/setgid/sticky.
- mtime: random offsets within +/- 10 years of `now`. Sub-second
  precision toggled per protocol version (29+ has nanosecond mtime).
- Symlinks: random target paths, including absolute, relative, dangling,
  and explicit cycle pairs (`a -> b`, `b -> a`). Cycle detection in
  the harness rejects cycles longer than depth 8.
- Hardlink groups: random partitioning of regular files into groups
  of size 1-N. Groups of size 1 mean no hardlinks; large groups
  exercise the hardlink hash table.

### Flag-set mutation

A "tagged universe" of flags. Each tag declares mutual-exclusion
constraints (e.g. `--inplace` excludes `--delay-updates`,
`--append` excludes `--partial-dir`, both already validated at
`CoreConfig` build time). The harness samples a flag combination by
rejection sampling: pick K flags, verify the conjunction is feasible,
retry on conflict. Infeasible combinations are pre-pruned to keep the
acceptance rate above 50%.

## Equivalence oracles

Per iteration, after both implementations finish, the harness asserts:

1. **Exit code identical.** Upstream and oc-rsync map errors to the
   same numeric codes (`ExitCode` in `crates/core/src/exit_code.rs`).
   Mismatch is always a divergence.
2. **Destination tree byte-identical.** Walk both destination trees in
   sorted-path order. For each entry compute
   `sha256(file_contents) || (mode & 07777, mtime_ns, uid, gid)`.
   Compare the resulting sorted digest stream. Symlinks compare on
   target path and mtime only. Special files compare on (mode,
   rdev, mtime).
3. **Wire bytes identical.** Daemon and SSH modes capture the
   application-layer payload via `tcpdump` (or a SOCKS proxy in the
   container). The capture is filtered to the rsync TCP port and
   diffed byte-for-byte. The capture-replay infrastructure already
   tracked in #2075 is the model. Local-copy mode skips this oracle
   (no wire payload).
4. **Stderr structurally equivalent.** Not byte-identical (timestamps
   and PIDs differ). Compare:
   - line count,
   - presence of role trailers per #996-#999
     (`[sender]`, `[receiver]`, `[generator]`, `[server]`,
     `[client]`, `[daemon]`),
   - error-code mentions (`(code N)` patterns).

   A divergence in any of those is an oracle failure. Lexical
   differences in human-readable text are not.

The oracles are independent; a single iteration can fail any subset.
The harness reports each oracle separately so a wire-bytes regression
does not mask a destination-tree regression.

## Implementation sites

### New crate: `crates/fuzz-harness/`

- `Cargo.toml` declares it `publish = false`, member of the workspace
  but excluded from `cargo build --workspace --release` via a
  workspace-level `default-members` filter (so release artifacts
  never embed the harness).
- Library entry point exposes a `Scenario` struct, a `run_pair(...)`
  function that drives both binaries, and an `Oracle` enum with the
  four assertions above.
- `cargo-fuzz` integration under `crates/fuzz-harness/fuzz/`. Targets:
  `fuzz_local_copy`, `fuzz_daemon_push`, `fuzz_daemon_pull`,
  `fuzz_ssh_push`, `fuzz_ssh_pull`, `fuzz_batch`. Each target reads
  bytes from libFuzzer, decodes via `arbitrary` into a `Scenario`,
  calls `run_pair`, panics on oracle mismatch.

The crate sits **outside** the production dependency graph
(`cli -> core -> ...`). Nothing in `core/`, `engine/`, `protocol/`,
`transfer/`, or any other production crate gains a dependency on it.
It is a peer of `xtask/`.

### Test runner: `tools/fuzz/run_protocol_diff.sh`

- Picks a random seed (or accepts `--seed <n>` for reproduction).
- Generates a corpus of N scenarios (default 100 for CI, 10 000 for
  nightly).
- Runs each scenario against both binaries.
- Captures wire payload via `tcpdump -i lo -w - port <p>` for
  daemon/SSH modes.
- Diffs outputs and writes a per-scenario verdict file.
- On any divergence, dumps the full scenario plus both binaries'
  stdout/stderr/wire-pcap into `target/fuzz-fail/<seed>/`.

The script reuses the daemon startup logic in `tools/ci/run_interop.sh`
(`ensure_workspace_binaries`, the daemon spawn block around the
`oc_pid_file_current` / `up_pid_file_current` variables) so daemon
lifecycle stays consistent.

### Container

The harness extends the existing `rsync-profile` long-running container
described in the global memory note `feedback_container_debug_endpoint.md`:

- Both `oc-rsync` and upstream `rsync 3.4.1` already pre-built.
- Add `cargo-fuzz` and `libfuzzer` to the image.
- Bind-mount the workspace at `/workspace` (already present) but
  **never** write fuzz output back to the bind-mount. Per the
  Containers & Bind Mounts pitfall, all destructible state lives
  under `/tmp/oc-rsync-fuzz/`, which is a tmpfs inside the container
  with a per-iteration `tempfile::TempDir` root.
- The script entry point is `tools/fuzz/run_protocol_diff.sh` mounted
  read-only.

## Corpus management

### Seed corpus

The `tools/ci/run_interop.sh` matrix is the seed:

- Each fixed scenario (push, pull, with delete, with compression,
  with hardlinks, with size-only, with numeric-ids, with exclude,
  with inplace; cited per the `Comprehensive interop tests` note in
  the memory file) becomes a seed `Scenario`. The seed is checked
  into `crates/fuzz-harness/fuzz/corpus/<target>/`.
- Existing golden scenarios in
  `crates/protocol/tests/golden_protocol_v28_handshake.rs`,
  `golden_protocol_v28_wire.rs`,
  `golden_protocol_v28_flist.rs`,
  `golden_protocol_v28_mplex_delta_stats.rs`,
  `golden_protocol_v29_flist.rs`,
  `golden_protocol_v29_wire.rs`, and `golden_handshakes.rs` are
  decoded into `Scenario` shapes and added to the seed corpus.

Seeding from known-good scenarios accelerates libFuzzer's coverage
ramp and keeps the early iterations on plausible territory.

### Reduction

When a fuzz run finds a divergence:

1. The scenario file (the libFuzzer input bytes) is captured under
   `target/fuzz-fail/<seed>/`.
2. `cargo fuzz tmin --runs=10000 fuzz_<target> <input>` shrinks the
   input to a near-minimal reproducer.
3. The shrunk input is normalized (re-decoded into the human-readable
   `Scenario` representation, re-encoded canonically) so the
   regression file is reviewable, not opaque bytes.

### Regression

Each shrunk failure becomes a permanent test under
`tests/protocol_fuzz_regressions/`:

- `tests/protocol_fuzz_regressions/<id>.scenario.json` (canonical
  `Scenario`).
- `tests/protocol_fuzz_regressions/<id>.expected.json` (oracle
  outputs from upstream).
- A `#[test]` in `tests/protocol_fuzz_regressions.rs` calls
  `harness::replay(<id>)`. The test runs in CI on every PR.

This mirrors the cargo-fuzz workflow recommended in the official
docs: shrink to a corpus entry, promote to regression test, never
delete.

## Wire-compat invariant

The harness has zero impact on production code paths. Every assertion
runs **outside** the binaries:

- No new flag in `crates/cli/`.
- No new module under `crates/core/`, `crates/engine/`,
  `crates/protocol/`, `crates/transfer/`, `crates/daemon/`,
  `crates/transport/`, or any other production crate.
- No `#[cfg(fuzz)]` guards in production code.
- The `crates/fuzz-harness/` crate depends inward on `protocol` (and
  helpers from `metadata`, `compress`) only for parsing tcpdump
  captures and decoding wire bytes; it never injects into them.

This invariant matches the discipline already documented in
`docs/design/zsync-inspired-matching.md` lines 21-34: harness changes
must leave golden tests, interop matrix, and tcpdump captures
byte-identical.

## Resource budget

### Per iteration

- ~10 MB on disk in `/tmp/oc-rsync-fuzz/<scenario_id>/`. Two trees
  (source + destination) plus tcpdump pcap + stderr captures.
- ~100 MB peak RSS. Bounded by the 10 000-file-tree cap and the
  random-bytes generator (largest single file 1 MB by default).

### Per CI run

- 100 iterations per PR. Soft wall-clock cap 10 minutes on a
  GitHub-hosted runner. Fail-fast on first divergence.
- 10 000 iterations nightly. Wall-clock cap 4 hours. Reports
  aggregate pass rate; first 10 divergences per oracle are uploaded
  as artifacts.

### Tempfile cleanup contract

- Every scenario root is a `tempfile::TempDir`, dropped at the end of
  the scenario regardless of pass/fail. The cleanup discipline
  documented at #1306-#1307 (tempdir hardening) carries over: a
  `Drop` impl removes the tree, and the harness installs an
  `atexit`-style guard for SIGINT / SIGTERM so a kill -9 of the
  runner does not orphan ~1 GB of fuzz state on the runner.
- The container `/tmp/oc-rsync-fuzz/` is a tmpfs. Container restart
  guarantees a clean slate.

## Failure handling

When the harness panics on either side or any oracle returns
divergence:

1. The full input (the libFuzzer corpus entry) is preserved.
2. Both binaries' stdout, stderr, and exit codes are captured.
3. The wire pcap (if any) is preserved.
4. The destination trees from both runs are tarred up and stored.
5. Everything goes into `target/fuzz-fail/<seed>-<oracle>/`.

The panic-isolation pattern from #1849 is reused: each scenario runs
in a child process spawned via `std::process::Command`. A panic in
the harness library (e.g. an oracle bug) terminates the child only;
the parent harness logs the panic, marks the scenario as
HARNESS_BUG (distinct from DIVERGENCE), and moves on. This keeps a
single bug from masking 9 999 valid runs.

## Activation in CI

The harness is opt-in. Default CI (`ci.yml`) does not run it. Three
activation surfaces:

1. **Manual dispatch.** A new `.github/workflows/protocol-fuzz.yml`
   declares `on: workflow_dispatch` and runs N=100 against the
   seed corpus + N=900 random. Modeled on the existing
   `Regenerate Golden Files (Manual)` job in
   `.github/workflows/interop-validation.yml` lines 6-31, which is
   already structured as opt-in.
2. **Nightly schedule.** Cron `0 6 * * *` runs N=10 000 in the
   container; failures open auto-issues via `gh issue create`.
3. **Per-PR.** A separate workflow with `if:` guard on a label
   (`fuzz`) runs N=100 against PRs that need the extra coverage
   (large protocol changes). Not on every PR; the cost is too high
   for that.

The fast wire-byte fuzz from #1194 stays where it is (in the
short-running CI matrix) and is unaffected.

## Risks and mitigations

### Non-determinism false positives

Two implementations can produce non-identical-but-correct outputs
when:

- mtime drift across the boundary of `time(2)` calls.
- Inode ordering differs across filesystems / kernel versions.
- Rsync's quick-check skipping (same mtime + size = skip) flickers
  on borderline cases.

Mitigations:

- Pin `--modify-window=1` per iteration. Both peers tolerate 1 s of
  mtime jitter.
- Sort all directory walks explicitly by name before comparison;
  never rely on `readdir(3)` order.
- Backdate destination files in the comparator by 1 day before the
  scenario runs (mirrors the test-flakiness mitigation in the
  Known Pitfalls section of the project notes).

### Memory blowup

A 10 000-file tree of all-`0xff` 1 MB files is 10 GB. Multiplied by
two implementations and a tcpdump capture, a single iteration can
exhaust runner RAM.

Mitigations:

- Per-run resource cap: `ulimit -v` at 4 GB inside the harness.
- File-size cap: max 1 MB per file (configurable). The generator
  bounds the total tree size at 100 MB regardless of file count.
- Tcpdump capture rotates to disk, never buffers in memory.

### Test-environment dependencies

Both binaries must be available, with matching feature flags
(zstd, lz4, ACLs, xattrs, io_uring). A missing feature flag causes
oracle false positives.

Mitigations:

- The container provides both binaries at known versions.
- The harness queries `--version` from each binary at startup and
  records a feature-flag manifest. Iterations that exercise an
  unavailable feature are skipped (counted as `SKIPPED`, not
  `PASS`, so the pass-rate metric stays honest).

## Interaction with #1303

#1303 tracks the wire-format **differential** fuzzer: corrupt one
byte mid-stream, verify both implementations either accept-equal or
reject-equal. That is byte-level mutation of an in-flight wire
payload.

This harness covers behavioural-level random scenarios (full
end-to-end transfers with random inputs). The two are layered:

- #1303 finds parser crashes and reject-vs-accept divergences on
  malformed wire bytes.
- This harness finds semantic divergences on well-formed inputs.

Both share the `Scenario` representation: #1303's "starting wire
payload before mutation" is generated by serializing a `Scenario`
through the production encoder, then a libFuzzer mutator flips a
byte. So the corpus produced here directly seeds #1303.

## Tracking

Four follow-up TODOs (listed for visibility, not added to the
persistent tracker):

1. Implement the `crates/fuzz-harness/` crate skeleton: `Scenario`
   struct, `run_pair`, six cargo-fuzz targets, integration with
   the existing `tempfile::TempDir` cleanup discipline.
2. Seed the corpus from the existing interop matrix
   (`tools/ci/run_interop.sh`) and the golden tests in
   `crates/protocol/tests/golden_*.rs`.
3. Add the `.github/workflows/protocol-fuzz.yml` workflow modeled
   on the `Regenerate Golden Files (Manual)` job.
4. Build the regression-extraction pipeline:
   `tools/fuzz/extract_regression.sh` that takes a fuzz-fail
   directory, runs `cargo fuzz tmin`, normalizes the input to
   `tests/protocol_fuzz_regressions/<id>.scenario.json`, and
   commits.

## References

- `crates/protocol/fuzz/` (#1194) - existing wire-byte fuzz harness.
- `crates/protocol/tests/golden_protocol_v28_handshake.rs`,
  `golden_protocol_v28_wire.rs`, `golden_protocol_v28_flist.rs`,
  `golden_protocol_v28_mplex_delta_stats.rs`,
  `golden_protocol_v29_flist.rs`, `golden_protocol_v29_wire.rs`,
  `golden_handshakes.rs` - static wire-format goldens.
- `crates/protocol/src/version/constants.rs` lines 7-16 - protocol
  version bounds and binary-handshake boundary.
- `crates/protocol/src/version/select.rs` - mutual-version selection.
- `crates/filters/` - filter rule grammar and parser.
- `tools/ci/run_interop.sh` lines 50-65 - fixed interop matrix
  seed.
- `.github/workflows/interop-validation.yml` lines 6-31 - opt-in
  manual-dispatch workflow pattern.
- `docs/design/zsync-inspired-matching.md` lines 21-34 - wire-compat
  invariants discipline.
- #996-#999 (role trailers), #1100 / #1379-#1381 (compression codec
  set), #1303 (wire-format differential fuzzer), #1306-#1307
  (tempdir cleanup), #1528-#1529 (path encoding hardening),
  #1849 (panic-isolation pattern), #2075 (tcpdump capture/replay).
