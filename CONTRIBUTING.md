# Contributing to oc-rsync

Thanks for your interest in contributing. This document captures the
day-to-day workflow used by the maintainers so that outside contributors can
match the same conventions. For project architecture see
[`README.md`](./README.md) and [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md);
for security disclosures see [`SECURITY.md`](./SECURITY.md).

---

## Local development workflow

oc-rsync uses a **push-and-let-CI-verify** workflow. Local builds are kept to
the minimum needed for a fast, lock-free edit/push cycle; the full validation
matrix runs in CI on every push.

### Required local commands (lock-free, fast)

- `cargo fmt --all` - always run before pushing.
- `cargo fmt --all -- --check` - pre-push sanity check; matches the CI gate.
- Standard `git` tooling for diff and history inspection.

### CI-only commands (do NOT run locally)

Leave these to CI. Running them locally is not required and is actively
discouraged because concurrent invocations have caused multi-minute build-lock
hangs on shared workstations:

- `cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings`
- `cargo nextest run --workspace --all-features`
- `cargo build` / `cargo check` against the full workspace

CI runs the full matrix (fmt + clippy, nextest on stable, Windows, macOS,
Linux musl) on every push. If you need to reproduce a failure locally, scope
the run to a single crate and test pattern:

```sh
cargo nextest run -p <crate> --all-features -E 'test(<pattern>)'
```

Use `cargo-nextest` rather than `cargo test`; configuration lives in
`.config/nextest.toml`.

---

## Adding a new optional dependency

1. Add the crate to the relevant per-crate `Cargo.toml`, under either
   `[dependencies]` or a platform-conditional table such as
   `[target.'cfg(target_os = "linux")'.dependencies]`, with `optional = true`
   and a matching entry in `[features]`.
2. Run `cargo update` once so the new entry is registered in `Cargo.lock`.
   This will also rewrite unrelated lock entries with patch-level bumps; that
   is normal cargo behaviour - accept the diff as-is.
3. For platform-conditional optional deps, mirror the `iouring-send-zc`
   pattern in `crates/fast_io/Cargo.toml`: declare the dep `optional = true`
   under a `[target.'cfg(...)'.dependencies]` block, then expose it as a
   named feature that re-exports the underlying capability.

---

## Opening a pull request

- **Branch naming.** Use `<category>/<short-description>[-<task-id>]`, for
  example `feat/parallel-delta-2087` or `docs/contributing-push-and-ci-workflow`.
- **Conventional prefixes.** Both commit messages and PR titles must use one
  of: `feat:`, `fix:`, `perf:`, `docs:`, `chore:`, `style:`, `test:`,
  `refactor:`, `ci:`. A labeler workflow auto-applies release-note categories
  from the PR title, so the prefix is load-bearing.
- **Title length.** Keep PR titles under 70 characters; put detail in the body.
- **CI gate.** All required checks must pass before merge: fmt + clippy,
  nextest (stable), Windows, macOS, Linux musl. PRs require one approving
  review. Master is protected; merge via GitHub (`gh pr merge`).
- **Authorship hygiene.** PR titles, PR bodies, branch names, commit messages,
  and added files must not reference internal tooling or non-human authoring
  aids. Use hyphens (`-`) rather than em-dashes in prose.

---

## EnvGuard CI lint

If you write a test that sets an `OC_RSYNC_BUFFER_POOL_*` env var (or
otherwise mutates `BufferPool` capacity state), wrap the env-var
manipulation in `EnvGuard` (from `platform::env::EnvGuard`). The CI lint
at `tools/ci/check_envguard.sh` walks every `#[test]` and
`#[tokio::test]` body in the workspace and flags any cap-touching test
that does not also hold an `EnvGuard`.

The lint runs in the `fmt + clippy` job. It is currently in warn-only
mode and will be flipped to strict (via `OC_RSYNC_ENVGUARD_LINT_STRICT=1`)
once BPF-3 (#2821) closes the existing gap. To silence a known-safe
test, add a `<repo-relative-path>::<fn_name>` entry to
`tools/ci/envguard_lint.ignore`. See
`docs/design/bpf-4-envguard-ci-lint-spec.md` for the full design and the
EOL plan; the lint is removed once BPF-9 replaces the global `OnceLock`
singleton with a per-test factory.

---

## Sandboxing primitives

When adding or modifying daemon-side code that admits a filesystem path
(`module.path`, `ref_dir`, `--temp-dir`, `--partial-dir`, `--backup-dir`,
`--link-dest` / `--copy-dest` / `--compare-dest`), prefer kernel-enforced
confinement over hand-rolled checks. The defense layers, in decreasing
strength:

1. **rust-landlock** (preferred). The `landlock` Cargo feature wires
   [rust-landlock 0.4](https://github.com/landlock-lsm/rust-landlock) through
   `fast_io::landlock::restrict_to_module_paths(&[...])`. Once engaged on the
   per-connection receiver thread, every path-based syscall outside the
   supplied roots returns `EACCES` from the kernel, regardless of which
   syscall userspace chose or how the path was resolved. This is the only
   layer that survives a future commit accidentally calling `std::fs::*` or a
   raw `libc::*` path syscall outside `DirSandbox`. Linux 5.13+ only;
   best-effort ABI downgrade picks the highest level the running kernel
   exposes (v3 on 6.2+, v2 on 5.19+, v1 on 5.13+). The stub returns
   `LandlockOutcome::Unavailable` on non-Linux targets and on pre-5.13 Linux
   per the IKV-F runtime-fallback contract, so callers never need to gate
   their code on kernel version.
2. **`openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`** via the `DirSandbox`
   carrier in `crates/fast_io/src/dir_sandbox/`. Closes the per-call TOCTOU
   window on every `*at` helper and is the first line of defense whenever
   Landlock is unavailable.
3. **Lexical `..` rejection.** Last-resort string-level check; necessary but
   never sufficient. Never rely on it as the sole barrier.

### Checklist when admitting a new root

Any change that introduces a new admission path - a new `ref_dir`, a new
`--temp-dir` / `--partial-dir` operand wiring, a new `--*-dest` cache, a new
out-of-tree backup target - must:

- Confirm the SEC-1.p ruleset built at
  `crates/daemon/src/daemon/sections/module_access/transfer.rs::engage_landlock_sandbox`
  includes the new root (or document why the new root must remain client-side
  only and is rejected at the wire-protocol layer, matching the existing
  `--temp-dir` / `--partial-dir` / `--backup-dir` posture).
- Route every mutation through `DirSandbox` so the `*at` helper layer also
  covers the root.
- Reference: SEC-1.p design at
  [`docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md`](./docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md);
  packager guidance at
  [`docs/packaging/landlock-feature-guidance.md`](./docs/packaging/landlock-feature-guidance.md).

Do not invent a new confinement primitive. If the existing layers do not
cover a case, extend `fast_io::landlock` or `fast_io::dir_sandbox` rather
than adding a parallel check.

---

## Parallel work pattern

- **Worktrees recommended.** Use `git worktree add` so each in-flight branch
  has its own checkout. This keeps cargo build state isolated per branch and
  avoids the lock contention that arises when several shells or agents share
  one target directory.
- **Cross-tree fix bundles allowed.** If a flake or regression on master is
  blocking several in-flight PRs, fix it in whichever branch is closest to
  mergeable rather than spinning up a separate hotfix PR.

---

## License

By contributing you agree that your contributions will be licensed under
GPL-3.0-or-later, matching the project license in [`LICENSE`](./LICENSE).
