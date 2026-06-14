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

## Cargo.lock maintenance

`Cargo.lock` is committed to the repository and every CI workflow (interop,
parallel-determinism, MSRV, release-cross, nightly platform jobs) builds with
`--locked`. The same flag is now required on `tools/ci/*.sh` helpers that
invoke cargo. The discipline exists so that:

- **Builds are reproducible.** A given commit always resolves to the exact
  same dependency graph, locally and in CI.
- **Dep drift is caught early.** A workflow that builds without `--locked`
  can silently absorb a transitive bump and mask a real regression.
- **PR diffs stay focused.** Lockfile churn lives in its own commit, not
  spread across unrelated PRs.

### When to update Cargo.lock

Update the lockfile only via an intentional `cargo update --workspace`.
Never let it drift sideways through a missing `--locked` flag - that path
re-resolves the graph implicitly and is the bug class this discipline
prevents.

### How CI enforces it

- `tools/ci/check_locked_flags.sh` (gating job, see CIM-LOCKFILE-5) scans
  every workflow and helper script for cargo invocations missing `--locked`
  and fails the PR if any unexpected call slips through.
- A small allowlist of cargo calls is intentionally exempt - the weekly
  refresh and PR auto-sync workflows below, which must run *without*
  `--locked` to do their job. The allowlist lives inside
  `tools/ci/check_locked_flags.sh`; add to it only with reviewer signoff.

### Weekly auto-sync

`.github/workflows/cargo-lockfile-weekly.yml` runs every Monday and opens a
`chore(deps): weekly Cargo.lock refresh` PR with the output of
`cargo update --workspace`. Contributors do not need to bump the lockfile
manually for routine drift - just review and merge the cron PR when CI is
green. `.github/workflows/cargo-lockfile-sync.yml` additionally pushes a
refreshed `Cargo.lock` back to any first-party PR that edits a workspace
`Cargo.toml`, so adding a dependency does not require a separate lockfile
commit.

### What to do if CI fails on Cargo.lock

If the gating job or a `--locked` workflow rejects your PR with a lockfile
mismatch, regenerate locally and commit the result:

```sh
cargo update --workspace
git add Cargo.lock
git commit -m "chore: sync Cargo.lock"
```

This is the same command the weekly cron and PR auto-sync workflows run; a
manual run from your branch is equivalent.

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
