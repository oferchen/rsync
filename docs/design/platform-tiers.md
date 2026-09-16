# Platform support tiers - criteria and promotion procedure

Status: measured 2026-09-15 against the `master` branch ruleset and the
workflows under `.github/workflows/`. This document defines what each
support tier means, scores the three operating systems against those
criteria, and states what a promotion requires procedurally.

Related documents:

- `README.md` (section "Platform Support") - the user-facing tier table.
- `docs/user/platform-support.md` - per-target detail, I/O matrix,
  release-artifact table.
- `docs/user/windows-support-matrix.md` - per-feature Windows cells with
  source citations.
- `docs/audits/win-tier2-stub-inventory.md` - the structural rationale for
  the current Windows tier.

Authority order when documents disagree: the branch ruleset and the
workflow files win, then this document, then the user-facing summaries.
The required-check list is never authoritative as prose. Re-derive it with
the `gh api .../rulesets` query in `docs/contributing/TESTING.md` (section
"CI Requirements") instead of trusting any copy, including the one below.

## 1. The tier ladder

Three tiers. A platform sits in the highest tier whose criteria it meets
in full. Each criterion is measurable and names its evidence source.

### Tier 1 - full platform

Every criterion below holds. The bar is derived from what CI already
enforces for Linux and macOS today, not from aspiration.

| # | Criterion | Measure | Evidence source |
|---|-----------|---------|-----------------|
| T1-1 | Required native gate | A stable-toolchain check on the master ruleset compiles and runs tests on a native runner for this OS | Ruleset query in `docs/contributing/TESTING.md`; job definitions in `.github/workflows/ci.yml` |
| T1-2 | Native test scope | The platform's CI cells run every crate that carries platform-specific code for this OS (at minimum: core, engine, cli, plus the metadata, I/O, transfer, and daemon crates that own its `#[cfg]` arms) | Per-job `cargo nextest run -p ...` package lists in `.github/workflows/ci.yml` |
| T1-3 | Upstream-testsuite legs | The pinned upstream corpus (3.5.0) runs in CI on this OS, on both transports (stdio pipe and loopback TCP daemon), against committed expected-result manifests so drift reddens the leg | `_upstream-testsuite*.yml` workflows; manifests under `tools/ci/upstream-3.5.0-expect.*.txt` |
| T1-4 | Interop harness | An interop job against upstream rsync binaries runs on every PR on this OS and its failures are visible (not `continue-on-error`) | `ci.yml` jobs `interop-upstream`, `interop-upstream-macos`, `interop-upstream-windows`; `_interop*.yml` |
| T1-5 | Metadata fidelity | ACLs, xattrs, and special files either round-trip with upstream-equivalent semantics, or every dropped entry produces a counted outcome (a warning plus a typed skip counter or a soft exit-code contribution) - never a silent drop | `crates/metadata/`; the counted-skip strategy documented in `docs/user/windows-support-matrix.md` (section "Notes on --devices, --specials, -D") |
| T1-6 | Path confinement | The receiver's destination-sandbox mechanism is implemented natively for this OS and its escape class is covered by an audit or by kernel enforcement | `crates/fast_io/src/dir_sandbox/mod.rs` (Unix carrier); `docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md` (Windows handle model) |
| T1-7 | Privilege model | The daemon can drop privileges to a configured identity on this OS | `crates/platform/src/privilege.rs` |
| T1-8 | Release artifacts | Tagged releases build and publish binaries for this OS on the stable toolchain | `.github/workflows/release-cross.yml` |
| T1-9 | Documentation accuracy | A support matrix for this OS exists and its cells cite shipped source locations that still match the tree | `docs/user/platform-support.md`; `docs/user/windows-support-matrix.md` |

Linux is additionally the reference platform: it alone runs the full
workspace under nextest as a required check, gates merges on the interop
and upstream-testsuite contexts, and hosts the fuzzing and benchmark
workflows. Those extras define the reference platform, not the tier -
otherwise macOS would not qualify for Tier 1.

### Tier 2 - supported with counted gaps

All of the following hold:

- T1-1 (required native gate) and T1-8 (release artifacts) hold in full.
- Core transfer modes (local copy, client push/pull over SSH and daemon
  TCP) build and pass their native CI cells.
- Every parity gap is documented in a support matrix with a source
  citation, and every dropped entry at runtime is counted, not silent.

Tier 2 permits: missing upstream-testsuite legs, best-effort interop,
lossy metadata translation, and feature stubs - provided each is
documented and counted.

### Unsupported

Anything else. No native CI, no release artifact, no behavioral claim.
Cross-compiled build-only targets (for example `aarch64-unknown-linux-*`)
are release artifacts without native test execution; they inherit
correctness from the Tier 1 source and are listed separately in
`docs/user/platform-support.md`, outside this ladder.

## 2. Current state, measured

Scored 2026-09-15. Each cell states what the named file does today.
"Gating" means the context appears in the master ruleset output of the
`TESTING.md` query; on that date the ruleset listed ten contexts: fmt +
clippy, nextest (stable), Windows (stable), macOS (stable), Linux musl
(stable), interop, and the four Linux upstream-testsuite legs.

| Criterion | Linux | macOS | Windows |
|-----------|-------|-------|---------|
| T1-1 required native gate | Yes - `nextest (stable)` and `Linux musl (stable)` gating (`ci.yml` jobs `test`, `linux-musl`) | Yes - `macOS (stable)` gating (`ci.yml` job `macos-test`) | Yes - `Windows (stable)` gating (`ci.yml` job `windows-test`) |
| T1-2 native test scope | Yes - full workspace, all features (`ci.yml` `test` job: `cargo nextest run --workspace --all-features`) | Partial - required cell runs core, engine, cli, metadata, apple-fs, fast_io; transfer and daemon crates are not in any macOS cell | Partial - required cell runs core, engine, cli, then metadata, fast_io, transfer; daemon crate runs in the separate non-gating `windows-daemon` job |
| T1-3 upstream-testsuite legs | Yes - four legs ({nonroot, root} x {pipe, tcp}), all four gating, manifests committed (`_upstream-testsuite.yml`) | Yes (non-gating) - four legs run on every PR with committed macOS manifests (`_upstream-testsuite-macos.yml`); registering the contexts as required is pending repo-admin action | No - no Windows upstream-testsuite workflow exists |
| T1-4 interop harness | Yes - gating (`interop / interop with upstream rsync`, `_interop.yml`) | Yes (non-gating) - smoke harness against Homebrew rsync runs on every PR (`_interop-macos.yml`); not in the ruleset | Partial - best-effort smoke against MSYS2 rsync with `continue-on-error: true` (`_interop-windows.yml`), so a failure is invisible on the PR |
| T1-5 metadata fidelity | Yes - POSIX ACLs, xattrs, devices, FIFOs (`exacl`, `crates/metadata/`) | Yes - ACLs, flat-namespace xattrs, AppleDouble resource forks (`crates/apple-fs/`) | Partial - NTFS DACL mapping drops deny ACEs, inherited ACEs, the SACL, and non-rwx bits with a one-time warning, not a per-entry counted outcome (`crates/metadata/src/acl_windows/posix_map.rs`); xattrs ship via NTFS ADS and preflight accepts `--xattrs` when built with the `xattr` feature (`crates/cli/.../workflow/preflight.rs:244-252`); devices/FIFOs/sockets are counted skips with per-entry warnings |
| T1-6 path confinement | Yes - `*at` dirfd carrier, `openat2` `RESOLVE_BENEATH` on 5.6+ kernels, optional Landlock layer (`crates/fast_io/src/dir_sandbox/mod.rs`) | Yes - same carrier via portable `*at` fallback; no kernel layer, so the userspace guard is load-bearing | Partial - the dirfd carrier is `#[cfg(unix)]`; Windows relies on NTFS handle semantics per the SEC-1.l audit, which found path-based Win32 call sites that still re-resolve (`docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md`) |
| T1-7 privilege model | Yes - `chroot` + `setgroups`/`setgid`/`setuid` (`crates/platform/src/privilege.rs`) | Yes - same POSIX path | Partial - impersonation via `LogonUserW` + `ImpersonateLoggedOnUser` is implemented (`privilege.rs::drop_privileges_windows`); it requires a configured account name and is not a POSIX-equivalent irreversible drop |
| T1-8 release artifacts | Yes - gnu + musl, x86_64 + aarch64, deb/rpm/apk/tarball (`release-cross.yml`) | Yes - x86_64 + aarch64 tarballs (`release-cross.yml` job `macos`) | Yes - x86_64-msvc tarball and zip (`release-cross.yml` job `windows`) |
| T1-9 documentation accuracy | Partial - `docs/user/platform-support.md` lists `interop (macOS)` as a required check and omits the four upstream-testsuite contexts; re-derive from the ruleset | Partial - `README.md` states every macOS required cell runs the full nextest workspace; the workflow runs a six-crate subset | Partial - `docs/user/windows-support-matrix.md` (dated 2026-06-10) still records the `--xattrs` preflight rejection and long-path/reparse rows that predate later changes; needs a re-verification pass |

Windows also carries eight scheduled nightly workflows (case folding,
daemon, IOCP, long paths, NTFS ACL, OpenSSH, reparse symlinks, xattr/ADS
- `windows-nightly-*.yml`) plus non-gating PR jobs `windows-iocp`,
`windows-acl-xattr`, `windows-daemon`, and `windows-gnu-cross-check`.
These are depth, not gates; they do not satisfy T1-3 or T1-4.

Reading of the table: Linux meets every Tier 1 criterion and is the
reference platform. macOS meets the Tier 1 bar with two non-gating suite
legs and two documentation drift items. Windows meets the Tier 2 bar and
misses Tier 1 on T1-3 (no testsuite legs), T1-4 (failures invisible),
T1-5 (lossy and under-counted DACL translation), T1-6 (unaudited residual
path-based call sites), and T1-9 (stale matrix rows).

## 3. What promotion requires

A platform moves up one tier through a single PR series that ends in a
repo-admin ruleset change. The checklist, in order:

1. Every criterion row for the platform reads "Yes" in section 2,
   re-measured against the tree at promotion time. Partial cells are
   closed by shipped code or by a documented, counted outcome - never by
   reclassifying the criterion.
2. The platform's CI contexts exist and have a green history on `master`:
   for Tier 1 that means upstream-testsuite legs on both transports with
   committed expected-result manifests, and an interop job whose failures
   fail the check (no `continue-on-error`).
3. A repo admin registers the platform's stable contexts in the master
   ruleset. Verification is the `TESTING.md` query, run before and after,
   with the diff attached to the promotion PR.
4. The user-facing documents are updated in the same series:
   `README.md` tier table, `docs/user/platform-support.md`, and the
   platform's support matrix, each cell re-cited against shipped source.
5. Section 2 of this document is re-scored with a new measurement date.

Demotion follows the same procedure in reverse: if a criterion regresses
and stays red for a release cycle, the tier table is corrected rather
than the criterion relaxed.
