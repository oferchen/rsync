# Contributing to oc-rsync
Thanks for your interest in contributing. This document describes the
workflow and conventions the maintainers follow. For architecture see
[`README.md`](./README.md) and [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md);
for security disclosures see [`SECURITY.md`](./SECURITY.md).
---
## Ground rules
- **Upstream is the source of truth.** oc-rsync mirrors upstream rsync 3.5.1
  (protocol 33). Read the C source in `target/interop/upstream-src/rsync-3.5.1/`
  before changing behaviour, and match it exactly. Any deliberate deviation
  must be stated in the PR, not made silently.
- **No placeholders.** No `todo!`, `unimplemented!`, `TODO`, `FIXME` or `XXX`
  in tracked Rust sources. Use the issue tracker instead.
- **Every change ships tests.** Tests must be able to fail when the behaviour
  they protect changes.
- **Hyphens, not em-dashes,** in all prose: comments, docs and commit messages.

Fetch the upstream source if it is missing:
```sh
mkdir -p target/interop/upstream-src
curl -L https://download.samba.org/pub/rsync/src/rsync-3.5.1.tar.gz \
  | tar xz -C target/interop/upstream-src
```
---
## Build, lint and test
Run one cargo command at a time: cargo holds an exclusive build-directory
lock, and concurrent invocations queue behind it.
### Format and lint
```sh
cargo fmt --all -- --check
python3 tools/ci/check_rustfmt_all.py
cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings
```
`check_rustfmt_all.py` formats every tracked `.rs` file. `cargo fmt` reaches
only files in the `mod` tree, so it misses `include!()`-ed and orphaned files.
### Tests
Use `cargo-nextest`, never `cargo test`. Configuration lives in
[`.config/nextest.toml`](./.config/nextest.toml). Locally, run only the tests
your change touches; the full suite runs in CI:
```sh
cargo nextest run -p <crate> --all-features -E 'test(<pattern>)'
```
### Repository gates
Each of these runs in CI and blocks the PR:
```sh
cargo run --locked -p xtask -- no-placeholders
python3 tools/ci/unsafe_owner_deps.py gate
python3 tools/ci/dead_code_census.py gate
bash tools/ci/surface_diff.sh --check
cargo xtask citations
```
- `unsafe_owner_deps.py gate` fails on a new direct FFI dependency outside
  `fast_io` and `platform` (see [Unsafe code](#unsafe-code)).
- `dead_code_census.py gate` fails on a `pub fn` with no production caller and
  no test reference unless it is allowlisted with a reason and expiry.
- `surface_diff.sh --check` diffs user-facing surfaces (options, daemon
  directives, exit codes, message codes and more) against upstream 3.5.1 and
  fails on any divergence not declared in `tools/ci/surface_diff.accepted`. It
  needs the upstream source tree above.
- `cargo xtask citations` is described under
  [Upstream source citations](#upstream-source-citations).
### Cross-platform checks
`cargo run -p xtask -- cross-check` runs the required cross-platform gates in
sequence and prints a per-target summary:
1. `cargo clippy --locked --workspace --all-targets --all-features --no-deps -- -D warnings`
2. `cargo check --locked --workspace --target x86_64-pc-windows-gnu`
3. `cargo check --locked --workspace --target x86_64-unknown-linux-musl`

A target whose toolchain (mingw-w64, musl) is absent is skipped with a hint,
never reported as passing. windows-MSVC is excluded because it cannot be
cross-compiled; windows-gnu covers the same `cfg(windows)` paths. The required
Windows job also compiles tests, so check them for the crates you touch:
```sh
cargo check --target x86_64-pc-windows-gnu -p <crate> --all-features --tests
```
`cargo run -p xtask -- validate` is the full local pre-push run: the drop-in
fidelity matrix against upstream across every client transport, then the
cross-check above. It does not reformat, so run `cargo fmt --all` first.
---
## Platform and test rules
- **Code must build and pass on Linux, macOS and Windows.** Gate
  platform-specific code with `#[cfg(unix)]` / `#[cfg(windows)]`. Unsupported
  platforms or features get a stub that returns a graceful result or error,
  never silently drops the functionality.
- **Watch `cfg` fallout.** Imports, variables or `mut` used only under one
  platform warn on the others. If every test in a module is `#[cfg(unix)]`,
  gate the module itself.
- **Deterministic timing.** No sleeps, polling loops or wall-clock upper
  bounds in tests. Wait on events, or inject a clock seam for timing logic.
  Performance belongs in benchmarks, not test assertions.
- **No retries or waivers for flakes.** Fix the root cause. Do not add nextest
  retries, `#[ignore]` or a rerun to make a flaky test pass.
- **Quick-check.** rsync skips files with matching size and mtime. Backdate
  destination files (`filetime`) or use different sizes so a test cannot pass
  by skipping the transfer.
- **Environment isolation.** Wrap environment mutation in
  `platform::env::EnvGuard`. `tools/ci/check_envguard.sh` flags tests that
  touch `OC_RSYNC_BUFFER_POOL_*` without one; it runs warn-only in the
  `fmt + clippy` job. Exempt a known-safe test with a
  `<repo-relative-path>::<fn_name>` line in `tools/ci/envguard_lint.ignore`.
  Design and EOL plan: `docs/design/bpf-4-envguard-ci-lint-spec.md`.
---
## Unsafe code
Unsafe code belongs to two crates, split by subject:
- **`fast_io`** - I/O syscalls (io_uring, copy dispatch, reflink, the confined
  path resolver).
- **`platform`** - process, identity, environment and signals.

Every other crate root carries `#![deny(unsafe_code)]` and calls the safe APIs
of these two. Before writing `unsafe`, try in order: a safe wrapper crate
(`nix`, `rustix`, `socket2`, `windows`), a safe API on `platform` or `fast_io`,
and only then a new `#[allow(unsafe_code)]` function in an owner crate, with a
`SAFETY:` comment naming what was tried. Never add `#[allow(unsafe_code)]` to
a crate that denies it. `tools/ci/unsafe_owner_deps.py gate` enforces the
dependency side; existing exceptions are listed with a reason and expiry in
`tools/ci/unsafe_owner_deps_allowlist.tsv`.
---
## Upstream source citations
Annotate non-obvious protocol and wire behaviour with the upstream file and
function, e.g. `// upstream: sender.c:803 send_files() - "sender finished"`.
- **One pinned baseline.** Line numbers refer to the pinned tree
  `target/interop/upstream-src/rsync-3.5.1/`. Do not append a version to each
  citation; the baseline is recorded once, in
  `tools/ci/citation_drift_audit.py` (`VER`) and `docs/UPSTREAM_COMPARISON.md`.
- **Quote a distinctive anchor.** Include a short verbatim string or symbol
  from the cited line so the reference can be relocated when lines move.
- **The hard gate.** `cargo xtask citations` checks that every citation in the
  repository names a `.c`, `.h`, `.py` or `.sh` file the pinned release has,
  at a line that file has. It reads `tools/ci/upstream-3.5.1-lines.tsv`, so it
  needs no source tree. Citations under a directory the rsync tarball does not
  ship (zsync's `librcksum/`) are out of scope.
- **Naming a release.** `rsync-3.5.1/flist.c:2717` is accepted and checked as
  the bare name; prefer the bare name. Any other release, such as
  `rsync-3.4.1/delete.c:130`, fails as a stale citation. Only this file,
  `xtask/src/commands/citations.rs` and `docs/` may name another release.
- **On an upstream bump.** Update the source tree and `VER`, then list
  citations whose line no longer matches their anchor:
  ```sh
  python3 tools/ci/citation_drift_audit.py            # all crates
  python3 tools/ci/citation_drift_audit.py engine cli # scoped
  ```
  `tools/ci/citation_retarget.py` re-locates citations by enclosing function
  and line text, and lists the ones it cannot place. Never shift line numbers
  by a fixed offset. Regenerate the manifest with
  `cargo xtask citations --write-manifest` in the same change that moves `VER`.
---
## Dependencies and Cargo.lock
`Cargo.lock` is committed and CI builds with `--locked`, so builds are
reproducible and dependency drift cannot slip in unnoticed.
- **Adding an optional dependency.** Declare it `optional = true` in the
  crate's `Cargo.toml` (under a `[target.'cfg(...)'.dependencies]` table if it
  is platform-specific) and expose it as a named feature. See
  `iouring-send-zc` in [`crates/fast_io/Cargo.toml`](./crates/fast_io/Cargo.toml).
- **Updating the lockfile.** Only through an intentional
  `cargo update --workspace`, committed on its own:
  ```sh
  cargo update --workspace
  git add Cargo.lock
  git commit -m "chore: sync Cargo.lock"
  ```
- **Enforcement.** `tools/ci/check_locked_flags.sh` fails on any cargo
  invocation in a workflow or CI script that lacks `--locked`; its allowlist
  of exempt calls needs reviewer sign-off.
- **Automation.** Dependabot ([`.github/dependabot.yml`](./.github/dependabot.yml))
  opens grouped dependency PRs every Monday.
  [`cargo-lockfile-sync.yml`](./.github/workflows/cargo-lockfile-sync.yml)
  pushes a refreshed `Cargo.lock` to any first-party PR that edits a workspace
  `Cargo.toml`, and posts the patch as a comment on fork PRs.
---
## Opening a pull request
Master is protected: changes land only through a pull request, and force
pushes to master are blocked. Merge via GitHub (`gh pr merge`).
- **Branch naming.** `<category>/<short-description>`, for example
  `feat/parallel-delta` or `docs/contributing-refresh`.
- **Conventional prefixes.** Commit messages and PR titles start with a
  prefix. The labeler workflow (`.github/workflows/labeler.yml`) maps the PR
  title prefix to a label, which sets the release-note category:

  | Prefix | Label |
  |--------|-------|
  | `feat:` | `enhancement` |
  | `perf:` | `performance` |
  | `fix:` | `bug` |
  | `docs:` | `documentation` |
  | `ci:` | `ci` |
  | `test:` | `test` |
  | `refactor:` | `refactor` |
  | `chore:` | `chore` |
  | `style:` | `style` |
- **Length.** Keep the first line of a commit message and the PR title under
  72 characters; put detail in the body.
- **PR body.** Follow
  [`.github/PULL_REQUEST_TEMPLATE.md`](./.github/PULL_REQUEST_TEMPLATE.md).
- **Required checks.** All 15 must pass before merge:
  - `fmt + clippy`
  - `nextest (stable)`
  - `Windows (stable)`
  - `macOS (stable)`
  - `Linux musl (stable)`
  - `interop / interop with upstream rsync`
  - `upstream-testsuite / upstream testsuite`
  - `upstream-testsuite / upstream testsuite (root)`
  - `upstream-testsuite-tcp / upstream testsuite`
  - `upstream-testsuite-tcp / upstream testsuite (root)`
  - `upstream-testsuite-macos / upstream testsuite (macOS)`
  - `upstream-testsuite-macos / upstream testsuite (macOS, root)`
  - `upstream-testsuite-macos-tcp / upstream testsuite (macOS)`
  - `upstream-testsuite-macos-tcp / upstream testsuite (macOS, root)`
  - `unsafe-safety-comment-audit`
- **Authorship hygiene.** PR titles, bodies, branch names, commit messages and
  added files must not reference internal tooling or non-human authoring aids.
---
## Parallel work
Use `git worktree add` so each in-flight branch has its own checkout and build
state. If a flake or regression on master blocks several PRs, fix it in the
branch closest to mergeable rather than opening a separate hotfix PR.
---
## License
By contributing you agree that your contributions will be licensed under
GPL-3.0-or-later, matching the project license in [`LICENSE`](./LICENSE).
