# Compatibility flags mutual-exclusion matrix

Tracking issue: oc-rsync task #2106.

This audit narrows in on the **transfer-mode flag family** -
`--checksum`, `--checksum-choice`, `--checksum-seed`, `--inplace`,
`--whole-file` / `--no-whole-file`, `--append` / `--append-verify`,
`--partial` / `--partial-dir`, `--delay-updates`, `--temp-dir` - and
documents exactly which pairwise combinations upstream rsync 3.4.1
rejects, which it silently accepts, and where oc-rsync currently
diverges. It complements the existing protocol-level audits
(`compatibility-flags-matrix.md` for `CF_*` bit interactions and
`compatibility-flags-transfer-matrix.md` for the broader transfer-flag
survey) by giving the conflict table its own first-class source of
truth.

Last verified: 2026-05-14 against `origin/master`.

## Sources cross-checked

- Upstream rsync 3.4.1:
  - `target/interop/upstream-src/rsync-3.4.1/options.c` - popt table
    entries at `:706-766`, conflict-checking block at `:2382-2428`,
    server-arg emission at `:2933-2942`.
  - `target/interop/upstream-src/rsync-3.4.1/checksum.c:185-204` -
    `CSUM_NONE` forcing `whole_file = 1`.
- oc-rsync validation sites:
  - `crates/core/src/client/config/builder/mod.rs:269-296` -
    `ClientConfigBuilder::validate` (the client-side gate that
    rejects `--inplace` / `--append` against `--partial-dir` or
    `--delay-updates`).
  - `crates/transfer/src/config/builder.rs:430-460` -
    `ServerConfigBuilder::validate` (transfer-side gate; rejects
    `--inplace` vs `--delay-updates` and `--append` vs
    `--partial-dir`).
  - `crates/engine/src/local_copy/options/builder/validation.rs:9-75` -
    `LocalCopyOptionsBuilder::validate` (local-copy path; rejects
    `--size-only` vs `--checksum` and `--inplace` vs
    `--delay-updates`).
  - `crates/core/src/client/config/builder/tests.rs:1055-1116` -
    conflict-validation tests.
- Status legend: **enforced** = oc-rsync rejects the combination at
  build time; **suppressed** = oc-rsync accepts but post-processes
  one flag away (matching upstream's silent fix-up); **permissive** =
  oc-rsync accepts the combination where upstream rejects;
  **n/a** = no conflict in either implementation.

## 1. Per-flag semantics

Each entry summarises (a) the upstream semantics, (b) the wire /
protocol gating if any, and (c) oc-rsync's plumbing for the flag.

### 1.1 `--checksum` / `-c`

- **Semantics.** Force a full-file checksum comparison instead of the
  default quick-check (mtime + size). The flag does not affect the
  delta algorithm itself.
- **Wire impact.** No direct `CF_*` bit. The seed ordering used when
  hashing follows `CF_CHKSUM_SEED_FIX` (bit 5) on protocol >= 30.
  Algorithm choice (MD4 / MD5 / XXH3 / XXH128) is negotiated via the
  vstring exchange gated by `CF_VARINT_FLIST_FLAGS` (bit 7).
- **oc-rsync.** Stored as `ClientConfig::checksum`
  (`crates/core/src/client/config/builder/mod.rs:178`), emitted as
  the `c` capability char in the server arg
  (`crates/core/src/client/remote/flags.rs:67`). No mutual-exclusion
  rule is attached to `--checksum`.

### 1.2 `--checksum-choice` / `--cc` (and `--checksum-seed`)

- **Semantics.** Override checksum algorithm (`--checksum-choice`) or
  seed value (`--checksum-seed`). Upstream popt entries:
  `options.c:740-741` (`--checksum-choice`, `--cc`) and
  `options.c:835` (`--checksum-seed`).
- **Wire impact.** When `--checksum-choice=none` (`CSUM_NONE`) is
  selected, upstream `checksum.c:197-198` forces `whole_file = 1`
  because the delta algorithm requires a strong checksum. The choice
  is also subject to the `do_negotiated_strings` vstring exchange.
- **oc-rsync.** Stored as `ServerConfigBuilder::checksum_choice`
  (`crates/transfer/src/config/builder.rs:75`) and
  `ClientConfigBuilder::checksum_choice`
  (`crates/core/src/client/config/builder/mod.rs:179`). No
  validator promotes `whole_file` when the chosen algorithm is
  `None`. See gap G1 in section 4.

### 1.3 `--inplace`

- **Semantics.** Write directly into the destination file rather than
  staging a temp file and renaming. Upstream popt:
  `options.c:706-707`.
- **Wire impact.** Combines with `CF_INPLACE_PARTIAL_DIR` (bit 6)
  introduced at protocol 30 to allow `--partial-dir` to coexist with
  `--inplace`. Without that bit, upstream rejects the pair at
  `options.c:2406-2414`.
- **oc-rsync.** Stored in three places: `ClientConfig` (line 229 of
  the builder), `ServerConfig`
  (`crates/transfer/src/config/builder.rs` -> `write.inplace`), and
  `LocalCopyOptions::inplace`
  (`crates/engine/src/local_copy/options/builder/setters_staging.rs`).

### 1.4 `--whole-file` / `--no-whole-file` / `-W`

- **Semantics.** Skip the delta algorithm and send the file literally.
  Tri-state in upstream (`whole_file` defaults to `-1` at
  `options.c:46`; positive forces, zero disables, negative leaves the
  heuristic to local-copy detection). Popt entries
  `options.c:734-736`.
- **Wire impact.** No `CF_*` bit. Emitted as the `W` capability char
  in the server-arg string (`options.c:2622-2624`), suppressed when
  `--append` is active (handled by the conflict gate, see 4.).
- **oc-rsync.** Stored as `ClientConfig::whole_file` (`Option<bool>`)
  at `crates/core/src/client/config/builder/mod.rs:177`. Server-arg
  emission at `crates/core/src/client/remote/invocation/builder.rs:540`
  and `crates/core/src/client/remote/flags.rs:96-98` (the latter
  comment explicitly cites `options.c:2622` and notes that `W` is
  suppressed when `--append` is active).

### 1.5 `--append` / `--append-verify`

- **Semantics.** Append to the receiver's existing data; only the
  byte range past the existing tail is transferred. `--append-verify`
  additionally re-checksums the basis range. Upstream popt:
  `options.c:708-710`. `--append` increments `append_mode` (1 on the
  client, `++` on the server: `options.c:1706-1711`); `--append-verify`
  sets it directly to 2.
- **Wire impact.** No `CF_*` bit. Forced to mode 2 at proto < 30 (see
  upstream `compat.c`). The server-args emission at
  `options.c:2933-2936` emits `--append` once (client) or twice
  (server, indicating verify).
- **oc-rsync.** `ClientConfig::append` and `append_verify` at
  `crates/core/src/client/config/builder/mod.rs:230-231`. The
  `ServerConfigBuilder::flags.append` field (`builder.rs:441`) is the
  enforcement gate.

### 1.6 `--partial`

- **Semantics.** Keep partially transferred files on interruption so a
  rerun can resume. Receiver-local option; no wire footprint. Popt:
  `options.c:761` (not shown above but follows the `--partial-dir`
  block).
- **Wire impact.** None.
- **oc-rsync.** `ClientConfig::partial` at
  `crates/core/src/client/config/builder/mod.rs:222`. Implicitly
  enabled when `--partial-dir` is supplied
  (`crates/core/src/client/config/builder/tests.rs:910-914`).

### 1.7 `--partial-dir=DIR`

- **Semantics.** Stage partial files under `DIR` until the transfer
  completes. Popt: `options.c:764`.
- **Wire impact.** No bytes on the wire; the receiver still consults
  the filter chain for `DIR`. Coexistence with `--inplace` requires
  `CF_INPLACE_PARTIAL_DIR` (bit 6).
- **oc-rsync.** `ClientConfig::partial_dir`
  (`crates/core/src/client/config/builder/mod.rs:223`),
  `ServerConfigBuilder::has_partial_dir`
  (`crates/transfer/src/config/builder.rs:79`).

### 1.8 `--delay-updates`

- **Semantics.** Stage every updated file in a `.~tmp~` directory and
  rename them all at the end of the run, so an interrupted run does
  not leave a half-updated tree. Popt:
  `options.c:765-766`. Upstream silently sets
  `partial_dir = tmp_partialdir` when `--delay-updates` is supplied
  without an explicit `--partial-dir` (`options.c:2403-2404`).
- **Wire impact.** No wire bytes.
- **oc-rsync.** `ClientConfig::delay_updates`
  (`crates/core/src/client/config/builder/mod.rs:228`),
  `ServerConfigBuilder::write.delay_updates`
  (`crates/transfer/src/config/builder.rs:260-262`).

### 1.9 `--temp-dir=DIR` / `-T`

- **Semantics.** Override the staging directory used for the
  conventional (non-`--inplace`) rename-into-place transfer. Popt:
  `options.c:813` (client) and `:864` (server). Upstream validates
  only the path length (`options.c:2158-2161`) - it does **not** mark
  `--temp-dir` as incompatible with `--inplace`; the staging dir is
  simply unused when in-place writes are active.
- **Wire impact.** Emitted as `--temp-dir <path>` in server args
  (`options.c:2907-2909`).
- **oc-rsync.** `ClientConfig::temp_directory`
  (`crates/core/src/client/config/builder/mod.rs:224`),
  `LocalCopyOptions::temp_dir`
  (`crates/engine/src/local_copy/options/builder/definition.rs:112`).
  Not part of any mutual-exclusion check; matches upstream.

## 2. Mutual-exclusion conflict matrix

Cells are read row-against-column. Each entry records the *outcome
upstream emits* and then how *oc-rsync emits the same combination*.
Citations point to the validating C / Rust source.

Legend:

- `OK` - both implementations accept the pair.
- `ERR` - both implementations reject with a build-time error.
- `OK->fix` - upstream silently fixes one side (e.g. promotes
  `partial_dir` to `tmp_partialdir`). oc-rsync mirrors the fix-up.
- `OK*` - oc-rsync accepts where upstream rejects (gap).
- `-` - diagonal entry (flag against itself).

| (row x col) | `--checksum` | `--checksum-choice=none` | `--inplace` | `--whole-file` | `--no-whole-file` | `--append` | `--append-verify` | `--partial` | `--partial-dir=DIR` | `--delay-updates` | `--temp-dir=DIR` |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `--checksum` | - | OK | OK | OK | OK | OK | OK | OK | OK | OK | OK |
| `--checksum-choice=none` | OK | - | OK | OK->fix [a] | ERR [a] | OK | OK | OK | OK | OK | OK |
| `--inplace` | OK | OK | - | OK | OK | OK [b] | OK [b] | OK [c] | ERR [d] | ERR [e] | OK [f] |
| `--whole-file` | OK | OK->fix [a] | OK | - | ERR (tri-state) | ERR [g] | ERR [g] | OK | OK | OK | OK |
| `--no-whole-file` | OK | ERR [a] | OK | ERR (tri-state) | - | OK | OK | OK | OK | OK | OK |
| `--append` | OK | OK | OK [b] | ERR [g] | OK | - | OK | OK [c] | ERR [h] | ERR [i] | OK |
| `--append-verify` | OK | OK | OK [b] | ERR [g] | OK | OK | - | OK [c] | ERR [h] | ERR [i] | OK |
| `--partial` | OK | OK | OK [c] | OK | OK | OK [c] | OK [c] | - | OK->fix [j] | OK->fix [j] | OK |
| `--partial-dir=DIR` | OK | OK | ERR [d] | OK | OK | ERR [h] | ERR [h] | OK->fix [j] | - | OK [k] | OK |
| `--delay-updates` | OK | OK | ERR [e] | OK | OK | ERR [i] | ERR [i] | OK->fix [j] | OK [k] | - | OK |
| `--temp-dir=DIR` | OK | OK | OK [f] | OK | OK | OK | OK | OK | OK | OK | - |

Footnote validation sites:

- [a] `--checksum-choice=none` forces `whole_file = 1` upstream
  (`target/interop/upstream-src/rsync-3.4.1/checksum.c:197-198`). A
  caller-supplied `--no-whole-file` therefore contradicts the algorithm
  choice. Upstream emits no error in `options.c`; the contradiction
  manifests later in `parse_checksum_choice`. oc-rsync currently
  performs neither the silent promotion nor the rejection: see gap G1.
- [b] `--append` sets `inplace = 1` upstream
  (`options.c:2392`). The `--inplace` and `--append`/`--append-verify`
  pair is therefore redundant but accepted (`options.c:2406`). oc-rsync
  treats `is_inplace = self.inplace || self.append`
  (`crates/core/src/client/config/builder/mod.rs:277`), so the pair
  produces the same effective state without error.
- [c] `--inplace` implies `--partial` for refusal purposes (upstream
  `options.c:2415-2421`, `keep_partial = 0`). The pair is accepted.
  oc-rsync accepts the pair by absence of any check
  (`crates/core/src/client/config/builder/mod.rs:276-296`).
- [d] Upstream rejects `--inplace --partial-dir` unless
  `CF_INPLACE_PARTIAL_DIR` is negotiated. The conflict is detected at
  `options.c:2406-2414`. oc-rsync now exposes
  `ClientConfigBuilder::validate_with_capabilities` (see
  `crates/core/src/client/config/builder/mod.rs`), which permits the
  pair when the negotiated bitfield contains
  `CompatibilityFlags::INPLACE_PARTIAL_DIR`. `validate()` keeps the
  strict default for callers that have not yet seen the negotiated
  bits, matching upstream's parse-time refusal. Gap G2 is resolved.
- [e] Upstream rejects `--inplace --delay-updates` at
  `options.c:2406-2414` (the `partial_dir` message string covers both
  paths because `delay_updates` synthesises a `partial_dir` upstream).
  oc-rsync rejects the pair at
  `crates/core/src/client/config/builder/mod.rs:281-286` and at
  `crates/transfer/src/config/builder.rs:432-438` and at
  `crates/engine/src/local_copy/options/builder/validation.rs:17-22`.
- [f] `--inplace --temp-dir` is accepted everywhere. The temp dir is
  unused but not invalid (upstream `options.c:2158-2161` only checks
  the path length).
- [g] `--append --whole-file` is rejected upstream at
  `options.c:2382-2387` with the message `--append cannot be used with
  --whole-file`. **oc-rsync currently does not reject the pair**; it
  silently suppresses the `W` capability char at
  `crates/core/src/client/remote/flags.rs:96-99` and the local-copy
  path forces delta semantics at
  `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs:315`.
  See gap G3.
- [h] `--append --partial-dir` is rejected upstream
  (`options.c:2406-2414`, via the `--append` -> `inplace = 1`
  promotion). oc-rsync rejects the pair at
  `crates/core/src/client/config/builder/mod.rs:288-293` and at
  `crates/transfer/src/config/builder.rs:441-446`.
- [i] `--append --delay-updates` is rejected upstream
  (`options.c:2406-2414`, same promotion path). oc-rsync rejects the
  pair at `crates/core/src/client/config/builder/mod.rs:281-286`.
- [j] `--partial` and either `--partial-dir` or `--delay-updates` are
  accepted. Upstream sets `keep_partial = 1` whenever `partial_dir` is
  non-NULL, and sets `partial_dir = tmp_partialdir` when
  `delay_updates` is supplied without an explicit `partial_dir`
  (`options.c:2403-2404`). oc-rsync mirrors this:
  `partial_directory(Some(...))` flips `partial = true`
  (`crates/core/src/client/config/builder/tests.rs:910-914`), but the
  `delay_updates -> partial_dir = .~tmp~` promotion is performed at
  the engine layer rather than the config builder. See gap G4.
- [k] `--delay-updates --partial-dir=DIR` is accepted. Upstream
  honours `DIR` for the staging tree (`options.c:2434-2438`).
  oc-rsync accepts the pair by absence of any check.

## 3. Where validation lives in oc-rsync

The conflict checks above are distributed across three builders. This
is intentional - upstream rsync routes the same option set through the
client-process popt parse, the server-process popt parse, and the
local-copy fast path - but it means a behavioural fix often needs to
land in more than one place.

| Site | Cells enforced | Source |
|------|----------------|--------|
| `ClientConfigBuilder::validate` | `--inplace` / `--append` vs `--partial-dir` / `--delay-updates` (cells d, e, h, i) | `crates/core/src/client/config/builder/mod.rs:269-296` |
| `ServerConfigBuilder::validate` | `--inplace` vs `--delay-updates` (cell e) and `--append` vs `--partial-dir` (cell h) | `crates/transfer/src/config/builder.rs:430-460` |
| `LocalCopyOptionsBuilder::validate` | `--inplace` vs `--delay-updates` (cell e) and `--size-only` vs `--checksum` (out of scope for this matrix) | `crates/engine/src/local_copy/options/builder/validation.rs:9-75` |

Conformance tests:

- `crates/core/src/client/config/builder/tests.rs:1058-1116` covers
  cells d, e, h, i (four rejection tests plus three accept-as-OK
  tests).
- `crates/engine/src/local_copy/options/builder/tests.rs` covers cell
  e on the local-copy path.

## 4. Divergences (gaps)

Ordered by user-facing impact.

### G1 - `--checksum-choice=none` does not promote `--whole-file`

- **Upstream behaviour.**
  `target/interop/upstream-src/rsync-3.4.1/checksum.c:197-198` sets
  `whole_file = 1` unconditionally when the negotiated transfer
  checksum is `CSUM_NONE`. Combined with
  `target/interop/upstream-src/rsync-3.4.1/options.c:1981-1987`, this
  means a user who passes `--checksum-choice=none` ends up with a
  whole-file transfer even if they did not pass `-W`.
- **oc-rsync behaviour.** `ChecksumAlgorithm::None` is recognised at
  `crates/transfer/src/shared/checksum.rs:146` and falls back to MD4
  semantics for seed compatibility, but no code path promotes
  `whole_file` to `Some(true)`. The pair `--checksum-choice=none
  --no-whole-file` therefore yields a delta transfer that has no
  strong checksum, which violates upstream's invariant.
- **Impact.** Wire divergence (oc-rsync runs the delta algorithm
  where upstream sends literal data); silent.

### G2 - `--inplace --partial-dir` cannot be enabled even when peer advertises `CF_INPLACE_PARTIAL_DIR` (RESOLVED)

- **Upstream behaviour.** `compat.c:777-778` enables `inplace_partial`
  when both peers negotiate `CF_INPLACE_PARTIAL_DIR` (bit 6,
  introduced at protocol 30). With that bit set, upstream's
  `options.c:2406-2414` rejection is bypassed at the receiver and the
  pair runs successfully.
- **oc-rsync behaviour.** `ClientConfigBuilder` now exposes
  `validate_with_capabilities(caps)` alongside the strict default
  `validate()`. When `caps` contains
  `CompatibilityFlags::INPLACE_PARTIAL_DIR`, the
  `--inplace`/`--append` + `--partial-dir` combination is accepted;
  otherwise the upstream parse-time rejection is preserved.
- **Status.** Resolved (#2142/#2143/#2144). The strict `validate()`
  default remains so callers without negotiated capability bits still
  match upstream's `options.c:2406-2414` behaviour.

### G3 - `--append --whole-file` accepted instead of rejected

- **Upstream behaviour.** `options.c:2382-2387` rejects the pair with
  `--append cannot be used with --whole-file`, exit code 1.
- **oc-rsync behaviour.** Accepted; the `W` capability char is
  silently suppressed at
  `crates/core/src/client/remote/flags.rs:96-99`. The user gets the
  `--append` semantics but no diagnostic.
- **Impact.** Permissive divergence. A user script that depends on
  upstream rejecting the combination (for example to fail-fast on a
  misconfigured cron job) will succeed unexpectedly under oc-rsync.

### G4 - `--delay-updates` promotion to `partial_dir = ".~tmp~"` happens after the config builder

- **Upstream behaviour.** `options.c:2403-2404` synthesises a partial
  directory when `--delay-updates` is supplied without
  `--partial-dir`. The synthesised value is then used everywhere
  downstream as if the user had supplied `--partial-dir=.~tmp~`.
- **oc-rsync behaviour.** The promotion is not performed in
  `ClientConfigBuilder` (the builder still sees
  `partial_dir = None`); it is reconstructed in the engine layer when
  the delay-updates rename batch is materialised
  (`crates/engine/src/local_copy/tests/execute_delay_updates.rs`).
  This is observable when a caller introspects the post-build config:
  `config.partial_directory()` returns `None` even though delay-updates
  is on. No wire divergence, but the API surface differs from
  upstream's effective state.
- **Impact.** Internal-API divergence; no user-visible effect on the
  wire.

## 5. Recommended next-fix

**Fix G3 (`--append --whole-file` rejection) first.** Rationale:

1. It is the only **permissive** divergence in the table. Permissive
   gaps risk silently shipping data that upstream would refuse - the
   exact failure mode the existing `cli-argument-parity-vs-rsync-manpage.md`
   audit prioritises.
2. The fix is localised. Add a single conflict cell to
   `ClientConfigBuilder::validate`
   (`crates/core/src/client/config/builder/mod.rs:276-296`) mirroring
   upstream's `options.c:2382-2387` message string, plus a regression
   test in `tests.rs:1055-1116`.
3. The fix needs no protocol or wire changes; it is purely a
   config-time check.
4. G1 (`--checksum-choice=none` -> `whole_file`) is tracked as a
   follow-up because it requires a checksum-negotiation callback.
   G2 (`CF_INPLACE_PARTIAL_DIR` runtime relaxation) was resolved by
   `validate_with_capabilities`, which threads the negotiated `CF_*`
   bits into the builder while keeping the strict default in place.
5. G4 is internal API only; defer until the rename-batch path is
   refactored independently.

Suggested commit shape for the G3 follow-up:
`fix: reject --append --whole-file at config build time (#2106)`.
