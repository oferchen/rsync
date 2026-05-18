# AGENTS.md

Tracking ledger for cross-cutting engineering series in the oc-rsync workspace.
This file records initiative-scoped work that spans multiple PRs so that future
contributors can navigate from a series code back to its audit, design notes,
and implementation history.

Pure documentation. No code changes are made by edits to this file.

## Completed initiatives

Each entry lists the series code, a one-line scope summary, the first audit or
design document to read, and the PR range that delivered the work.

- **MPE - Mutex poison recovery policy** (MPE-1..MPE-11, mostly shipped)
  - Policy doc: `docs/audits/mutex-poison-policy.md`
  - First classification: `docs/audits/mutex-poison-policy.md` (recovery
    classification landed via #2350, #2351, #2358)
  - See PRs #4341, #4359 in the #43XX..#44XX range.

- **ATU - Arc::try_unwrap fragility mitigation** (ATU-1..ATU-8, shipped)
  - Audit: `docs/audits/arc-try-unwrap-classification.md`
  - Channel-shutdown remediation landed via ATU-4.
  - See PRs #4338 and adjacent in the #43XX..#44XX range.

- **BGE - BGID exhaustion mitigation** (BGE-1..BGE-7, shipped)
  - Audit: `docs/audits/io-uring-bgid-exhaustion.md`
  - Lifecycle reference: `docs/audits/bgid-lifecycle.md`
  - Includes graceful degradation and a 50%-occupancy warning.
  - See PRs #4331, #4353, #4355 in the #43XX..#44XX range.

- **WAS - Windows ACL support** (WAS-1..WAS-8, shipped)
  - CI matrix: `docs/audits/windows-acl-xattr-ci-matrix.md`
  - `--acls` is wired end-to-end via the SDDL xattr slot.
  - See PRs around #2463 plus follow-ups in the #43XX..#44XX range.

- **SSE - SSH socketpair stderr** (SSE-1..SSE-8, shipped)
  - Audit: `docs/audits/ssh-socketpair-vs-pipes.md`
  - Verification: `docs/audits/ssh-socketpair-vs-anonymous-pipes-verification.md`
  - Shipped behind the `ssh-socketpair-stderr` cargo feature.
  - See PRs #4339, #4348, #4368, #4385 in the #43XX..#44XX range.

- **SMR - mmap vs SQPOLL** (SMR-1 shipped; SMR-2 in flight; SMR-3* pending)
  - Audit: `docs/audits/io-uring-sqpoll-mmap-interaction.md`
  - Design / decision framework: `docs/design/mmap-vs-sqpoll-conflict-resolution.md`
  - SMR-1 bench harness is shipped. SMR-2 decision framework is in flight.
    SMR-3* steps are pending hardware numbers.
  - See PRs around #4329, #4201 in the #43XX..#44XX range.

- **STN - SpillPolicy tunability** (STN-1..STN-15, mostly shipped)
  - Design: `docs/design/spill-policy-public-api.md`
  - User-facing docs landed via #2346 (see #4378).
  - Covers `spill_dir`, reclaim, granularity, compression, env vars, CLI
    flags, docs, benches, and unit tests.
  - See PRs #4340, #4360, #4378 in the #43XX..#44XX range.

- **SPL - spill.rs decomposition** (SPL-1..SPL-12, mostly shipped)
  - Plan: `docs/audits/spill-rs-decomposition-plan.md`
  - Design: `docs/design/reorderbuffer-spill-to-tempfile.md`
  - Extracted error, codec, and stats submodules with accompanying audits.
  - See PRs #4337, #4390 in the #43XX..#44XX range.

- **IUD - io_uring data path** (IUD-1..IUD-9, mostly shipped)
  - Coverage audit: `docs/audits/iouring-data-path-coverage.md`
  - Send design: `docs/design/iouring-send-data-path.md`
  - Receive design: `docs/design/iouring-receive-data-path.md`
  - SEND_ZC scaffolding: `docs/design/iouring-send-zc.md`
  - Writer and reader paths shipped behind cargo features with a
    byte-identical test and a perf bench.
  - See PRs in the #43XX..#44XX range (writer/reader + SEND_ZC scaffolding).

- **FCV - Fuzz coverage visibility** (FCV-1..FCV-9, mostly shipped)
  - Inventory and matrix: `docs/audits/fuzz-coverage-matrix.md`
  - Gap audit: `docs/audits/fuzz-coverage-gap-followups.md`
  - Adds multiplex / flist / zlib / zstd / varint fuzz targets and a CI
    workflow for nightly coverage reports.
  - See PRs #4335, #4336, #4344, #4347, #4351 in the #43XX..#44XX range.

- **WPG - Windows perf gap** (WPG-1..WPG-6, mostly shipped)
  - IOCP synchronous blocking audit: `docs/audits/iocp-sync-blocking-audit.md`
  - Hotspot methodology: `docs/audits/windows-iocp-hotspots-methodology.md`
  - Benchmarks: `docs/audits/windows-iocp-benchmark.md`,
    `docs/audits/windows-iocp-benchmark-plan.md`
  - TransmitFile primitive, page-aligned IOCP, and CQ auto-sizing all landed.
  - See PRs #4332, #4334, #4358, #4370 in the #43XX..#44XX range.

- **PRC - PR conflict resolution** (PRC-1..PRC-14, 23 tasks, shipped)
  - Audit: `docs/audits/prc-3a-dacl-posix-overlap.md`
  - A mass-merge cascade triggered ~50 PRs to merge. The 13 long-running
    parallel-feature PRs that conflicted heavily with the evolved master were
    each rebased via per-PR conflict analysis.
  - Net: 12 of 13 merged, 1 closed as superseded.
  - See PRs #4345, #4357, #4363, #4369, #4377, #4397, #4398, #4400, #4421,
    #4438, #4405, #4449, plus master-fixes #4452 and #4454.

- **CSP - Checksum SIMD perf** (CSP-1..CSP-2, shipped)
  - Audit: `docs/audits/csp-1-rolling-simd-checksum-sync-regression.md`
  - User-reported 1.5-1.7x regression vs upstream in `--checksum` mode. Root
    cause: per-iteration horizontal reduction in AVX2/SSE2/NEON loops;
    upstream keeps `s1`/`s2` in vector registers across the full stripe.
  - CSP-2 fix refactors all three arch paths to vector-register-resident
    loops. Expected 1.4-1.8x speedup.
  - See PR #4451.
