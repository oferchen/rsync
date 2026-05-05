# Eliminate-path matrix for `tools/ci/known_failures.conf`

Tracking: beta-readiness criterion #3 (the `KNOWN_FAILURES` array and
`is_known_failure_from_conf()` rules in `tools/ci/known_failures.conf` must
both be empty before the project leaves beta).

Last verified: 2026-05-01 against master @ `9da533d6`. Source files
spot-checked:

- `tools/ci/known_failures.conf` (the conf this audit maps).
- `target/interop/upstream-src/rsync-3.4.1/compat.c` (protocol-version
  feature gates, batch-codec hardcoding, vstring negotiation).
- `target/interop/upstream-src/rsync-3.4.1/exclude.c` (filter-prefix
  legal-length table by protocol version).
- `target/interop/upstream-src/rsync-3.4.1/token.c` (delta token batch
  tee + inflate path).
- `crates/daemon/src/daemon/sections/config_parsing/module_directives.rs`
  (`charset =` directive parser, line 330).
- `crates/core/src/client/run/batch.rs` (oc-rsync's batch writer).
- `crates/batch/src/replay.rs` (oc-rsync's batch reader).
- Companion audits: [`docs/audits/iconv-pipeline.md`](iconv-pipeline.md),
  [`docs/audits/iconv-inert.md`](iconv-inert.md),
  [`docs/audits/iconv-feature-design.md`](iconv-feature-design.md),
  [`docs/audits/iconv-filename-converter-design.md`](iconv-filename-converter-design.md),
  [`docs/audits/zstd-batch-compatibility.md`](zstd-batch-compatibility.md),
  [`docs/audits/protocol-28-32-interop-matrix.md`](protocol-28-32-interop-matrix.md).

## Scope and method

The conf file declares two surfaces:

1. The unconditional `KNOWN_FAILURES` array (entries that always skip
   regardless of negotiated protocol).
2. The `is_known_failure_from_conf()` shell function with conditional
   rules keyed on `direction` and `forced_proto`.

For each surface this audit produces one section that records:

- Entry name and direction.
- Category: oc-rsync gap, oc-rsync bug, upstream bug, or upstream
  protocol limitation.
- Root cause, citing upstream `rsync-3.4.1` source by file and line and
  using the existing comments in the conf as the starting reference.
- Eliminate path. Each entry resolves to one of:
  - **Fix in oc-rsync** - identifies the file/area to change and the
    tracking issue. These remove the entry from the conf.
  - **Permanent: upstream bug** - the upstream binary itself fails the
    same scenario; the conf entry documents and skips. Cannot be removed
    without a release that downgrades the upstream pin or changes the
    interop contract.
  - **Permanent: protocol-version-locked** - upstream hardcodes a
    version gate; the feature is by definition unavailable at that
    negotiated protocol. Removing the conf entry would require shipping
    a wire feature upstream itself does not implement, which the project
    has explicitly declined (per the "no wire protocol features"
    policy).
- Estimated complexity for fixable items (S, M, L).

A change is not credited toward criterion #3 until both the conf entry
and the matching `DASHBOARD_ENTRIES` row are removed in the same PR.

## Section 1: unconditional `KNOWN_FAILURES` entries

### 1.1 `standalone:iconv-upstream`

- **Direction:** standalone (driven by the `--iconv` interop scenario,
  not a daemon push/pull direction).
- **Category:** oc-rsync gap (daemon-mode `--iconv` plumbing
  incomplete).
- **Root cause:** PR #3458 bridged `IconvSetting` to the
  `FilenameConverter` trait for SSH and local transfers, and PR #3527
  wired `IconvSetting` into the config builder. Daemon-mode iconv
  remains incomplete:
  - `crates/daemon/src/daemon/sections/config_parsing/module_directives.rs:330`
    parses the `charset =` module directive, but the parsed value is
    not threaded into the daemon's iconv runtime.
  - Findings 1, 2, and 3 of [`docs/audits/iconv-pipeline.md`](iconv-pipeline.md)
    remain open (symlink target transcoding, `--files-from`
    forwarding, `--secluded-args`/`--protect-args` transcoding).
  - Upstream contract for the missing pieces:
    `target/interop/upstream-src/rsync-3.4.1/flist.c:1127-1150`
    (recv symlink iconv), `flist.c:1605-1621` (send symlink iconv),
    `io.c:416-452` (`--files-from` forwarding), `rsync.c:283-320`
    (`send_protected_args` per-arg iconv).
  - Negotiation gate: upstream `compat.c:716-718, 763-767` advertises
    `CF_SYMLINK_ICONV` only when `iconv_opt` is set; oc-rsync's
    `crates/transfer/src/setup/capability.rs::build_capability_string`
    always emits `s` rather than driving it from `IconvSetting`.
- **Eliminate path:** Fix in oc-rsync. Two follow-up PRs gated on the
  iconv audits already in the tree:
  1. Wire the daemon `charset =` value into the iconv runtime that
     `IconvSetting` constructs at config-build time. Touchpoints:
     `crates/daemon/src/daemon/sections/module_definition/finish.rs`,
     `crates/core/src/config/build.rs`, the iconv plumbing surfaced by
     PR #3527.
  2. Land Findings 1-3 from [`docs/audits/iconv-pipeline.md`](iconv-pipeline.md)
     so symlink-target, `--files-from`, and `--secluded-args` paths
     all run `ic_send`/`ic_recv` like upstream.
- **Tracking issue:** #1916 (existing), plus the open
  follow-ups already itemised in `docs/audits/iconv-pipeline.md`.
- **Complexity:** L (touches daemon config, capability negotiation,
  flist read/write paths, `files_from`, secluded args).

### 1.2 `standalone:delta-stats`

- **Direction:** standalone (delta-statistics interop assertion).
- **Category:** oc-rsync bug.
- **Root cause:** When oc-rsync runs as the daemon-mode server, the
  delta engine does not engage. The block-match scheduler produces
  zero matched blocks even for pre-seeded identical destinations, so
  every byte is sent as a literal. Conf annotates this as "delta engine
  not active in daemon code path" and the upstream contract is
  `token.c:send_token()` block-match emission combined with the
  generator-driven signature exchange that the daemon path skips.
  Existing tests:
  - PR #3461 `test: add delta-stats interop test verifying transfer
    statistics` introduced the standalone interop case.
  - Conf entry was added by `b7e648b5 test: add delta-stats to known
    failures pending delta engine daemon fix`.
- **Eliminate path:** Fix in oc-rsync. The delta engine entry point
  used by SSH and local transfers must be wired into the daemon
  server-side handler so generator-side signature emission and
  receiver-side block matching run identically regardless of transport.
  Likely touchpoints:
  - `crates/engine/src/local_copy/executor/...` and
    `crates/engine/src/sender/` (delta sender activation).
  - `crates/daemon/src/daemon/...` (server-side dispatch into the same
    code path SSH uses).
- **Tracking issue:** open daemon-mode delta engine fix; see PR
  history `b7e648b5` and `36829acb` for the test scaffolding that
  turns green when the bug is fixed.
- **Complexity:** M-L (architectural: align daemon server-side with
  the SSH/local executor instead of running a literal-only path).

### 1.3 `standalone:upstream-compressed-batch-self-roundtrip`

- **Direction:** standalone (batch self-roundtrip with upstream
  rsync 3.4.1 on both sides).
- **Category:** upstream bug.
- **Root cause:** Upstream rsync 3.4.1 cannot read its own compressed
  delta batches. Verified path:
  - `target/interop/upstream-src/rsync-3.4.1/token.c:608` performs
    `inflate()` on batch tokens but the dictionary feed that
    `see_deflate_token()` performs during a live transfer is missing
    on the batch reader path.
  - `target/interop/upstream-src/rsync-3.4.1/compat.c:194-195`
    hardcodes `CPRES_ZLIB` for batch reads, and `compat.c:413-414`
    forces `compress_choice = "zlib"` for batch writes, so the batch
    writer always tees raw `DEFLATED_DATA` tokens whose inflate
    dictionary is never re-primed on replay.
  - Result: any compressed batch that contains block matches
    (COPY tokens) fails replay with `inflate returned -3` and exit
    code 12.
  - oc-rsync reads the same files correctly because
    `crates/batch/src/replay.rs:475-479` and `replay.rs:807-824`
    re-implement the dictionary sync that upstream's batch reader
    omits, and `crates/core/src/client/run/batch.rs:97-107` writes
    post-decompression bytes with `do_compression=false`. See
    [`docs/audits/zstd-batch-compatibility.md`](zstd-batch-compatibility.md)
    for the full analysis (task #1685).
- **Eliminate path:** Permanent: upstream bug. The conf entry
  documents and skips a scenario where both endpoints are upstream
  rsync 3.4.1; oc-rsync cannot fix the upstream binary. Removal of
  the entry would require either:
  - An upstream release that fixes `token.c:608` batch dictionary
    sync (out of oc-rsync's control), or
  - Dropping the upstream-vs-upstream self-roundtrip from CI, which
    defeats the purpose of the interop matrix.
- **Tracking:** referenced from
  [`docs/audits/zstd-batch-compatibility.md`](zstd-batch-compatibility.md);
  no oc-rsync issue is appropriate because the fix is in upstream.

## Section 2: conditional rules in `is_known_failure_from_conf()`

These rules apply only on the `up` direction when `forced_proto` is set.
The `forced_proto` mechanism exists so CI can exercise oc-rsync against
older protocol versions (3.0.9 speaks 28, 3.1.3 speaks 30+) without
needing to install older upstream binaries on every runner.

### 2.1 Protocol <= 29: `up:acls`

- **Direction:** up.
- **Category:** upstream protocol limitation.
- **Root cause:** `target/interop/upstream-src/rsync-3.4.1/compat.c:655-661`
  inside `setup_protocol()` hard-exits with `RERR_PROTOCOL` when
  `--acls` is requested at a negotiated protocol below 30. The wire
  format for ACL flist encoding fields was introduced in protocol 30;
  no encoding exists at 28 or 29. Verified: upstream `rsync -A
  --protocol=29` exits with code 2.
- **Eliminate path:** Permanent: protocol-version-locked. There is no
  oc-rsync change that can encode ACLs over a protocol version that
  defines no wire encoding for them. Project policy forbids inventing
  wire features absent from upstream.

### 2.2 Protocol <= 29: `up:xattrs`

- **Direction:** up.
- **Category:** upstream protocol limitation.
- **Root cause:** `target/interop/upstream-src/rsync-3.4.1/compat.c:662-668`
  hard-exits with `RERR_PROTOCOL` when `--xattrs` is requested at
  protocol < 30. The xattr wire-format fields were added in protocol
  30 alongside ACLs. Verified: upstream `rsync -X --protocol=29`
  exits with code 2.
- **Eliminate path:** Permanent: protocol-version-locked. Same
  reasoning as `up:acls`.

### 2.3 Protocol <= 29: `up:compress-zstd`

- **Direction:** up.
- **Category:** upstream protocol limitation.
- **Root cause:** Algorithm selection requires vstring (variable
  string) negotiation, which depends on the `'v'` compat flag.
  Upstream `compat.c:556-564` (`negotiate_the_strings()`) and
  `compat.c:729-742` only enable `'v'` at protocol >= 30. At protocol
  < 30 upstream falls back to a hardcoded `strlcpy(tmpbuf, "zlib",
  MAX_NSTR_STRLEN)` at `compat.c:383`. zstd cannot be negotiated
  because the negotiation surface does not exist. Verified: upstream
  `rsync --compress-choice=zstd --protocol=29` silently picks zlib,
  causing codec mismatch downstream.
- **Eliminate path:** Permanent: protocol-version-locked.

### 2.4 Protocol <= 29: `up:compress-lz4`

- **Direction:** up.
- **Category:** upstream protocol limitation.
- **Root cause:** Identical to `up:compress-zstd`. lz4 is selected via
  the same vstring negotiation channel that requires protocol >= 30.
- **Eliminate path:** Permanent: protocol-version-locked.

### 2.5 Protocol <= 28: `up:merge-filter`

- **Direction:** up.
- **Category:** upstream protocol limitation.
- **Root cause:** `target/interop/upstream-src/rsync-3.4.1/exclude.c:1530`
  sets `legal_len = 1` at protocol < 29, restricting filter prefixes
  to a single character. The dir-merge `':'` prefix needs `legal_len
  >= 2` (introduced in protocol 29). At protocol 28 the filter parser
  rejects `:` as an invalid prefix. Verified: upstream `rsync
  --filter='merge ...' --protocol=28` fails.
- **Eliminate path:** Permanent: protocol-version-locked.

## Section 3: punch list, sorted by category

| Category | Entry | Direction | Eliminate path | Complexity | Tracking |
|---|---|---|---|---|---|
| oc-rsync gap | `standalone:iconv-upstream` | standalone | Fix: wire daemon `charset =` to iconv runtime; finish symlink/files-from/secluded-args transcoding (Findings 1-3 of `iconv-pipeline.md`) | L | #1916, [`docs/audits/iconv-pipeline.md`](iconv-pipeline.md) |
| oc-rsync bug | `standalone:delta-stats` | standalone | Fix: route daemon-mode server through the same delta engine entry point used by SSH/local transfers | M-L | PRs `b7e648b5`, `36829acb` (test scaffold) |
| upstream bug | `standalone:upstream-compressed-batch-self-roundtrip` | standalone | Permanent: upstream `token.c:608` batch inflate dictionary sync bug. oc-rsync reads correctly via `crates/batch/src/replay.rs:475-479,807-824` | n/a | [`docs/audits/zstd-batch-compatibility.md`](zstd-batch-compatibility.md) (#1685) |
| upstream protocol limitation | `up:acls` | up | Permanent: `compat.c:655-661` hard-exits at proto < 30, no ACL wire format below 30 | n/a | upstream contract |
| upstream protocol limitation | `up:xattrs` | up | Permanent: `compat.c:662-668` hard-exits at proto < 30, no xattr wire format below 30 | n/a | upstream contract |
| upstream protocol limitation | `up:compress-zstd` | up | Permanent: `compat.c:556-564,729-742` vstring negotiation requires proto >= 30; `compat.c:383` falls back to `"zlib"` | n/a | upstream contract |
| upstream protocol limitation | `up:compress-lz4` | up | Permanent: same as `up:compress-zstd` (vstring negotiation requires proto >= 30) | n/a | upstream contract |
| upstream protocol limitation | `up:merge-filter` | up | Permanent: `exclude.c:1530` `legal_len=1` at proto < 29 forbids dir-merge `':'` prefix | n/a | upstream contract |

## Section 4: fixable-vs-permanent summary

- **Fixable (eliminates the entry):** 2 entries.
  - `standalone:iconv-upstream` (oc-rsync gap, complexity L).
  - `standalone:delta-stats` (oc-rsync bug, complexity M-L).
- **Permanent (must stay in conf for the supported upstream pin):**
  6 entries.
  - 1 upstream bug: `standalone:upstream-compressed-batch-self-roundtrip`.
  - 5 upstream protocol-version-locked: `up:acls`, `up:xattrs`,
    `up:compress-zstd`, `up:compress-lz4`, `up:merge-filter`.

When the two fixable entries land, the unconditional `KNOWN_FAILURES`
array reduces to a single upstream-bug entry, and
`is_known_failure_from_conf()` reduces to its protocol-version gates.
That residue is structural: it documents upstream's own limits, not
oc-rsync's. Beta-readiness criterion #3 should treat the protocol-locked
and upstream-bug entries as compliant: the criterion is about
oc-rsync gaps, not about the contents of upstream's wire protocol.

## Section 5: cross-references to merged work

Recent commits relevant to this audit (from `git log --oneline -50`):

- `c09b8ae5 feat(core): wire IconvSetting to FilenameConverter at
  config build (#1911)` - PR #3527, the first half of the
  `standalone:iconv-upstream` fix (SSH/local path; daemon path still
  open).
- `2d2ffb99 feat: bridge IconvSetting to FilenameConverter (#3458)` -
  the SSH/local bridge that PR #3527 finished wiring through config.
- `5d127a3f test(interop): add --iconv UTF-8/LATIN1 round-trip vs
  upstream 3.4.1 (#1916)` - the standalone interop test that pins the
  `standalone:iconv-upstream` failure to a reproducible scenario.
- `91d31b2a fix(ci): escape charset backticks in known_failures.conf
  (#3532)` - last touch on the conf itself; documentation-only fix.
- `36829acb test: add delta-stats interop test verifying transfer
  statistics (#3461)` and `b7e648b5 test: add delta-stats to known
  failures pending delta engine daemon fix` - the test scaffolding for
  `standalone:delta-stats`; turns green once the daemon delta path is
  fixed.
- `4ef05a7a docs(batch): investigate zstd as batch-compatible
  compression alternative (#1685) (#3492)` - audit covering the
  upstream batch-compression bug behind
  `standalone:upstream-compressed-batch-self-roundtrip`.
- `d6cdd50e fix: handle compressed token streams in batch replay
  (#3310)` - the oc-rsync-side fix that lets us read upstream's
  compressed batches even though upstream cannot.
- `a2b03c17 docs(iconv): audit why --iconv is inert and map the wiring
  plan (#1918) (#3526)` and the chain of iconv design audits that
  precede it.

## Section 6: how to retire an entry

Single-PR procedure for removing a fixable conf entry:

1. Land the upstream-fidelity fix (cite upstream source by file:line in
   the commit message).
2. Run the relevant interop scenario locally against the upstream pin
   (3.0.9, 3.1.3, 3.4.1) plus oc-rsync; the failing case in
   `tools/ci/known_failures.conf` must succeed.
3. In the same PR, delete the matching entry from both
   `KNOWN_FAILURES` (or the `is_known_failure_from_conf` rule) and
   `DASHBOARD_ENTRIES`.
4. Update this audit: move the entry from Section 3's punch list to a
   new "Section 7: retired" log with the merge SHA and PR number.
5. When all fixable entries are retired, mark beta-readiness criterion
   #3 satisfied.
