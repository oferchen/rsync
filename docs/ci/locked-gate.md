# `--locked` regression gate

`Cargo.lock` is committed and every CI build is meant to honour it via
`--locked`. That guarantees byte-for-byte reproducible builds at a given
commit, surfaces transitive-dep drift in its own PR rather than letting
it ride along on an unrelated change, and keeps `cargo-lockfile-weekly`
as the single intentional path for dep bumps.

CIM-LOCKFILE-1..4 audited and fixed every cargo invocation in CI that
should carry `--locked`. The gate that prevents regressions is
`tools/ci/check_locked_flags.sh`, run as a step of
[`pr-lint.yml`](../../.github/workflows/pr-lint.yml) on every PR and on
push to master. It inspects every `.yml`/`.yaml`/`.sh` under
`.github/workflows/` and `tools/ci/` regardless of what changed in the
PR, so renames, merge-commit sneak-ins and drift in shell helpers are
all caught. An earlier diff-only fast gate covered a strict subset of
these files and was retired.

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
and document the rationale in the PR description.

Routine cargo additions never need an exemption: just pass `--locked`.
