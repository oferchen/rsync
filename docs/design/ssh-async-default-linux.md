# SSH Transport: Make `async-ssh` the Default on Linux

Tracking issue: #1890. Followup to `docs/design/ssh-transport-async-io-eval.md`
(the umbrella eval) and the hybrid recommendation it adopts (option (a) for
the daemon, option (b) for the CLI).

This document does not re-litigate which executor (tokio), which bridge
(`tokio::process::ChildStdin`/`ChildStdout` for the subprocess path,
`russh::ChannelStream` for embedded), or which migration sequencing
(`async-migration-plan.md`) the workspace has already committed to. It
narrows one open question:

> When does the synchronous SSH transport stop being the default on Linux,
> and what is the runway between "opt-in behind `--features async-ssh`" and
> "compiled in by default, used by default"?

The umbrella eval (section 8, "Promotion default") states the final answer:
"stay deferred" until five trigger conditions hold simultaneously. This
document sharpens those five into measurable gates, lists the Linux-only
surface that gets flipped, and writes the five-step rollout against the
existing tokio infrastructure.

## 1. Why Linux first

The umbrella eval treats the async surface as platform-uniform: tokio runs
on Linux, macOS, and Windows; `tokio::process::Child` reaps `SIGCHLD` on
Unix and synthesises named pipes on Windows for IOCP compatibility. The
platform-uniform story holds for correctness. It does not hold for the
performance gate.

Linux is the platform where:

- The async daemon listener (#1935) lands first and is already production
  scope; `daemon-tokio-async-listener-impl.md` is Linux-shaped.
- The SSH-async bench (`crates/rsync_io/benches/ssh_sync_vs_async.rs`,
  added in PR #4251) runs deterministically on the CI runners we control
  (the cell uses `tokio::task::spawn_blocking` over a userspace-throttled
  loopback `TcpStream`, not real `sshd`).
- io_uring (`crates/fast_io/src/io_uring/`) is the disk-side complement
  the async pump composes with; the cross-platform stub
  (`fast_io::io_uring_stub`) makes the pairing meaningless on non-Linux
  for the gate workloads.
- The thread-elimination win named in the umbrella eval section 2's
  fan-out row (`>= 20% wall clock`) is Linux-first because the rayon
  pool + blocking pool already share `num_cpus` accounting via the
  rayon bridge (#1751); macOS and Windows have not been measured.

Linux is the only platform where all five umbrella-eval triggers can be
observed against shipped code by the time #4242 (the async eval) lands.
Flipping the default elsewhere stays deferred behind the same feature
gate until each platform passes the same bench bar in section 4 below.

## 2. What "default on Linux" means concretely

Three layers of "default" exist; this section names the one we flip.

| Layer | State before | State after this PR's eventual implementation |
|-------|--------------|------------------------------------------------|
| `Cargo.toml` `default-features` | `embedded-ssh, async` already there; `async-ssh` absent | `async-ssh` added to `default-features` on Linux via a target-cfg |
| Runtime `RSYNC_ASYNC_SSH` env var | Honoured when set to `1`; unset means sync | Honoured; unset means async (Linux) / sync (other platforms) |
| Internal `SshTransport` dispatch | `SshConnection::connect_with_config` (sync) | `AsyncSshTransport::connect_with_config` (async) on Linux when `async-ssh` is active and the env var is not `=0` |

The flip is the third row: the `core::client::remote` dispatch switches
from the sync builder to the async builder under the Linux + `async-ssh`
cfg. The first row is the Cargo-level enabler. The second row is the
escape hatch users hit with `RSYNC_ASYNC_SSH=0` if their environment
trips a regression.

Crucially, the sync path stays compiled and tested. The flip is a default,
not a deletion. The umbrella eval section 8 trigger 1 (async daemon
listener stable for one release cycle) is the gate that lets us
contemplate sync-path deletion in a later release; this document does
not.

## 3. Trigger conditions, made measurable

The umbrella eval section 8 lists five triggers in prose. Each one is
re-stated below with the numeric bar that decides pass / fail and the
artifact in the tree that produces the measurement.

### 3.1 Trigger A: async daemon shipped and stable

**Bar**: #1935 merged and present in at least one tagged release. No
P0 or P1 issue filed against the async daemon listener for the duration
of one release cycle (typically 6 - 12 weeks). The release notes for the
cycle in question contain no `fix:`-prefixed entry that touches
`crates/daemon/src/` runtime code.

**Artifact**: `gh release view` for the prior tag; `gh issue list
--label bug --search "daemon async"` returns zero open at the time of
flip.

### 3.2 Trigger B: embedded russh covers the parity matrix

**Bar**: #1796 and #1797 merged. The embedded-ssh feature passes the
OpenSSH-parity matrix declared in `async-ssh-evaluation.md` open
question 1 (cipher set, kex algorithm set, host key types,
known-hosts file format, agent forwarding off, key-file auth on).
Specifically: the `crates/rsync_io/src/ssh/embedded/tests.rs` matrix
exits 0 against an upstream `sshd` 9.x in CI.

**Artifact**: a CI job under `.github/workflows/` named
`embedded-ssh-parity.yml` that runs the matrix on each PR; the job is
green at the time of flip.

### 3.3 Trigger C: SSH-async bench shows a real win

**Bar**: at least one row in `ssh_sync_vs_async.rs` shows
`async_spawnblocking_transfer/<size>` faster than
`sync_transfer/<size>` by `>= 10%` wall clock, measured at the
`SLOW_LINK_NS_PER_BYTE = 200` setting that approximates a transatlantic
RTT. No LAN row (with `SLOW_LINK_NS_PER_BYTE = 0`, runnable via an env
var override the bench will accept) regresses by more than 3%. The
result is reproducible across at least three CI runner classes
(`ubuntu-latest`, `ubuntu-22.04-arm`, the project's self-hosted Linux
runner if present).

**Artifact**: `cargo bench -p rsync_io --bench ssh_sync_vs_async`
JSON output checked into `target/criterion-history/` for the comparison
commits, plus a markdown summary appended to the release notes that
flip the default.

The bench today covers a narrow shape (loopback `TcpStream`,
`spawn_blocking` shim, not the real async pump). Trigger C must be
re-evaluated against the real pump (the `AsyncSshConnection` from
#1806 once it lands); the existing bench is a *lower bound* on the
win, not the final measurement.

### 3.4 Trigger D: runtime-flavour comparison

**Bar**: the umbrella eval section 7.1 runs, with all three variants
(sync, option (a) shared `rt-multi-thread`, option (b) per-connection
`current_thread`) measured. Hybrid recommendation holds: option (b)
within 5% of option (a) on the fan-out row, option (b) at least 50%
RSS reduction on single-connection CLI rows.

**Artifact**: a follow-up bench file
`crates/rsync_io/benches/ssh_runtime_flavour.rs` modelled on
`ssh_sync_vs_async.rs`, with the three variants as separate cells.
Result table in this document's section 8 (filled in when the bench
lands).

### 3.5 Trigger E: bridge-cost probe

**Bar**: the umbrella eval section 7.2 micro-bench shows
`tokio::process::ChildStdin`/`ChildStdout` within 5% of
`AsyncFd`-over-raw-FD. If the gap exceeds 5%, the escape hatch in
umbrella eval section 5.1 becomes a real fork that must ship before
the default flips, and trigger E gates on the fork shipping rather
than on the original ChildStdin path winning.

**Artifact**: micro-bench cell added to
`crates/rsync_io/benches/ssh_sync_vs_async.rs` (the existing file, to
avoid bench proliferation).

All five must be green simultaneously. A regression in any one row
reverts the default to sync via `Cargo.toml` and a `RSYNC_ASYNC_SSH=0`
hint in the release notes.

## 4. Migration: five steps

The umbrella eval section 9 sequences the five steps to *implement*
async SSH. This document sequences the five steps to *flip the default*
once that implementation has shipped.

1. **Add the Linux target-cfg `default-features` entry.** Edit the
   workspace `Cargo.toml` to add `async-ssh` to the binary crate's
   `[target.'cfg(target_os = "linux")'.dependencies]` `default-features`
   set. No code change. Gate: `cargo build --target
   x86_64-unknown-linux-gnu` produces a binary that links tokio in by
   default; `cargo build --target x86_64-apple-darwin` and
   `cargo build --target x86_64-pc-windows-msvc` do not (CI matrix
   verifies). Tracking: this PR's eventual implementation issue.

2. **Wire the runtime dispatch on Linux.** In
   `crates/core/src/client/remote/`, branch on
   `cfg(all(target_os = "linux", feature = "async-ssh"))` to call
   `AsyncSshTransport::connect_with_config` (from umbrella eval
   step 1) instead of `SshConnection::connect_with_config` (PR #4266).
   Gate: the interop harness (`tools/ci/run_interop.sh`) against
   upstream rsync 3.0.9, 3.1.3, 3.4.1 passes with the async dispatch.
   Cross-platform: macOS and Windows CI cells stay on the sync path.

3. **Add the `RSYNC_ASYNC_SSH=0` escape hatch.** At the dispatch
   point in step 2, honour `RSYNC_ASYNC_SSH=0` to force the sync path
   even when compiled in by default. Document the env var in
   `crates/cli/src/frontend/arguments/parsed_args/mod.rs` and the
   `oc-rsync(1)` man page generator (`xtask/src/commands/docs/`).
   Gate: a CLI integration test invokes both `RSYNC_ASYNC_SSH=0
   oc-rsync ...` and unset, asserts both succeed against a loopback
   sshd.

4. **Bench, fill the trigger table, ship behind a beta flag.** Run
   triggers C, D, E. If all pass, ship the Linux default flip in a
   release marked beta (per the `project_beta_status_notify.md`
   convention). Beta runtime is one release cycle minimum. Gate:
   release notes for that cycle contain zero `fix:` entries against
   `crates/rsync_io/src/ssh/` or `crates/core/src/client/remote/`.

5. **Promote to default-default.** Drop the beta marker; the Linux
   default is async. The sync path stays compiled (`--no-default-features`
   plus `--features sync-ssh` builds it, where `sync-ssh` is added in
   step 1 as the inverse default). Gate: clean release with no
   regressions filed against the async path during the beta cycle;
   the umbrella eval section 8 trigger 1 (one release cycle of
   stability) is satisfied for the async SSH path itself.

The five steps are strictly serial. Any step that fails its gate
stops the promotion and leaves the sync path as the Linux default.
A failed step does not undo the prior steps; the feature-gate
infrastructure remains in place for the next attempt.

## 5. Risk: existing sync-path users

The sync path is what every release before the flip has shipped. Users
in three classes carry the risk:

1. **Embedders.** Anyone using `oc-rsync` as a library inside their own
   tokio app already sees the sync transport. Flipping the default
   introduces a second tokio runtime in their process unless they opt
   into the hybrid daemon shape (umbrella eval option (a)). The escape
   hatch is `RSYNC_ASYNC_SSH=0` plus, for library users, a
   `disable_async_ssh()` builder method on the public `CoreConfig`
   surface that overrides the env var.

2. **CI users with locked-down network policies.** Tokio's mio reactor
   opens an epoll FD plus a small set of eventfd descriptors at runtime
   construction. Sandboxed CI environments that whitelist syscalls may
   refuse the construction outright. The mitigation is the
   `RSYNC_ASYNC_SSH=0` env var plus a clear error message at runtime
   construction failure that names the env var as the workaround.

3. **Users on long-lived SSH sessions with custom keepalive shapes.**
   The umbrella eval section 6.1 collapses the keepalive watchdog
   into `tokio::time::timeout`. Users who today set
   `RSYNC_SSH_KEEPALIVE_*` env vars (if any exist; verify before flip)
   keep the same behaviour because the watchdog implementation lives
   in `crates/rsync_io/src/ssh/connect.rs` and is preserved across
   the sync / async boundary.

The classes are mitigated, not eliminated. The cfg / feature gate
during rollout (steps 1 - 4 above) is the structural mitigation:
users who hit a regression flip back to sync without recompiling,
and the next release cycle decides whether the regression class is
real or one-off.

## 6. Open questions deferred to implementation

- The exact name of the inverse feature (`sync-ssh` vs `force-sync-ssh`)
  pending convention check against existing feature names in
  `Cargo.toml`. Punt to step 1's PR review.
- Whether `RSYNC_ASYNC_SSH` is the right env var name. Existing env
  vars use `OC_RSYNC_*` prefix
  (`OC_RSYNC_SSH_NET` in `connect.rs:446`); align step 3 with that
  convention. Likely final name: `OC_RSYNC_SSH_ASYNC`.
- Whether the Linux `default-features` toggle applies to musl as well
  as glibc. Probable yes; verify against the Linux musl CI cell when
  step 1 lands.

These are scoped to the implementation PRs, not to this design.
