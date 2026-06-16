# `--locked` regression gate

`Cargo.lock` is committed and every CI build is meant to honour it via
`--locked`. That guarantees byte-for-byte reproducible builds at a given
commit, surfaces transitive-dep drift in its own PR rather than letting
it ride along on an unrelated change, and keeps `cargo-lockfile-weekly`
as the single intentional path for dep bumps.

CIM-LOCKFILE-1..4 audited and fixed every cargo invocation in CI that
should carry `--locked`. CIM-LOCKFILE-5 added two gates that prevent
regressions:

1. **`check-locked-flags.yml`** (full scan): runs
   `tools/ci/check_locked_flags.sh` on every PR and on push to master.
   Inspects every `.yml`/`.yaml`/`.sh` under `.github/workflows/` and
   `tools/ci/` regardless of what changed in the PR.
2. **`locked-gate.yml`** (fast PR gate): runs only on PRs that touch
   `.github/workflows/**.yml`. Diffs HEAD against the PR base and only
   inspects the workflow files that actually changed. Pure bash + grep,
   no cargo invocation, finishes in seconds.

The two gates intentionally overlap. The fast gate fails first on the
common case of editing a workflow; the full scan catches anything the
fast gate misses (renames, sneak-ins via merge commits, drift in shell
helpers under `tools/ci/`).

## Gated subcommands

Every `cargo` invocation that runs one of these subcommands must carry
`--locked` on the same logical command (continuation lines via `\` are
joined before matching):

- `cargo build`
- `cargo check`
- `cargo clippy`
- `cargo run`
- `cargo test`
- `cargo nextest run`

`+toolchain` selectors are recognised: `cargo +nightly nextest run`
also requires `--locked`.

## Exempt subcommands

These subcommands are intentionally lock-free and the gate does not
inspect them:

| Subcommand | Reason |
|---|---|
| `cargo fmt` | Does not consume the workspace lockfile. |
| `cargo doc` | Already lock-aware where it matters (see `pages.yml`); semantically a docs build. |
| `cargo bench` | Benchmarks run against the workspace as resolved; lock state is incidental. |
| `cargo update` | The lockfile-mutation entry point itself (used by `cargo-lockfile-weekly.yml`). |
| `cargo tree`, `cargo metadata`, `cargo fetch` | Read-only inspection. |
| `cargo install` | Installs a third-party CLI; carries `--locked` where the upstream supports it, but the gate does not enforce. |
| `cargo xtask` | Workspace task runner; the underlying build is already gated. |
| `cargo deb`, `cargo generate-rpm` | Wraps an already-built artifact. |
| `cargo fuzz`, `cargo cov`, `cargo llvm-cov`, `cargo deny`, `cargo hakari`, `cargo publish` | Third-party plugins with their own resolution semantics. |

## Adding a one-off exemption

If a new invocation is genuinely lock-free (for example, a one-off
plugin whose `--locked` flag does not exist), add a `path:line` entry
to the `ALLOWLIST` array in
[`tools/ci/check_locked_flags.sh`](../../tools/ci/check_locked_flags.sh)
and document the rationale in the PR description. The fast PR gate
shares the same conceptual allowlist by virtue of failing on the same
condition; opting an invocation out means it stops appearing as a gated
match in both gates.

Routine cargo additions never need an exemption: just pass `--locked`.
