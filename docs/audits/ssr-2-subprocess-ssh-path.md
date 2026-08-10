# SSR-2: subprocess SSH path audit - keep / deprecate / remove

Status: KEEP (with hardening).
Scope: `crates/rsync_io/src/ssh/` and the `core` client transfer
dispatch in `crates/core/src/client/`. The `transport` crate referenced
in the SSR-2 brief does not exist as a separate workspace member; the
SSH transport lives entirely under `rsync_io::ssh`.

This audit is the SSR-2 companion to the
[`docs/ssh-transport-decision-matrix.md`](../ssh-transport-decision-matrix.md)
(SSR-3) policy doc and the SSR-4 goodbye-timeout guard
(`SSH_GOODBYE_TIMEOUT` in `crates/rsync_io/src/ssh/embedded/connect.rs`).

## 1. Why this audit exists

`v0.6.1` shipped a subprocess SSH push regression that hit a
goodbye-phase pipe-buffer deadlock and ran ~200x slower than upstream
on the release-benchmark workload. `v0.6.2` repaired the deadlock and
landed russh as the preferred SSH path. SSR-3 documented the matrix.
SSR-4 added the goodbye-timeout backstop. SSR-2 closes the loop by
deciding the future of the subprocess path itself: keep it as the
compatibility default, mark it `#[deprecated]` against a removal
window, or delete it outright.

## 2. Inventory: subprocess SSH path

| Concern                  | Value |
|--------------------------|-------|
| Default-build status     | **Default and only** path in release artifacts. `release-cross.yml` and `ci.yml` do not pass `--features embedded-ssh`. The workspace `default` feature set in `Cargo.toml` omits `embedded-ssh`. |
| Routing trigger          | `host:path`, `user@host:path`, `host::module` (when SSH is the transport), or **any** remote operand when the binary was built without `embedded-ssh`. See `crates/core/src/client/run/mod.rs:215-248`: only `ssh://` URLs are dispatched to `run_embedded_ssh_transfer`, every other remote operand falls through to `run_ssh_transfer`. |
| Entry point              | `core::client::remote::ssh_transfer::run_ssh_transfer` -> `build_ssh_connection` -> `rsync_io::ssh::SshCommand::spawn`. |
| Process model            | `std::process::Command::new("ssh")` with `Stdio::piped()` on stdin/stdout (see `builder.rs:361-362`). Stderr is socketpair-backed on Unix when `ssh-socketpair-stderr` is on, else `Stdio::piped()` (`aux_channel.rs:452-473`). |
| Stderr drain             | `aux_channel::PipeStderrChannel` plus the SSE-1..SSE-8 socketpair work. Default builds rely on the pipe drain; SSE-4 async-drain is opt-in. |
| ssh_config support       | Full. The spawned `ssh` binary reads `~/.ssh/config` and `/etc/ssh/ssh_config` directly. The crate parses a subset itself (`crates/rsync_io/src/ssh/config_lookup.rs`) only to detect the `Compression yes` double-compression case under the `ssh-config-parse` default feature. |
| `ProxyCommand`           | Honoured by the spawned `ssh` binary. The crate also surfaces it in operand parsing as an argv-injection guard (`operand.rs:81,137,157`). |
| `ControlMaster` / `ControlPath` / `ControlPersist` | Honoured by the spawned `ssh` binary. The crate does nothing extra. |
| `ProxyJump`              | Honoured by the spawned `ssh` binary. The builder also exposes `set_jump_hosts` so callers can forward `--jump-hosts` to `-J`. |
| `Match` / `Include`      | Honoured by the spawned `ssh` binary. |
| `ssh-agent` / hardware tokens | Honoured by the spawned `ssh` binary. No re-implementation. |
| Async variant            | `crates/rsync_io/src/ssh/async_transport.rs` (`AsyncSshTransport`). Opt-in behind `async-ssh`, runtime-enabled via `OC_RSYNC_ASYNC_SSH=1`. |
| Goodbye-phase deadlock   | Fixed in PR #4154 by keeping the stderr drain alive until the child exits. SSR-4 adds the embedded-side `SSH_GOODBYE_TIMEOUT` backstop. |
| Test coverage            | 263 tests in the subprocess path: 108 in `crates/rsync_io/src/ssh/tests.rs`, 60 in `config_lookup.rs`, 27 in `operand.rs`, 18 in `aux_channel.rs`, 12 in `connect.rs`, 9 in `parse.rs`, 8 in `async_stderr_drain.rs`, 5 in `async_transport.rs`, 3 in `socketpair_stderr.rs`, plus 12 in `tests/ssh_config_compression.rs`, 6 across the three `tests/ssh_stderr_*.rs` integration files, 8 in `core/tests/ssh_transfer.rs`, and 19 in `core/src/client/remote/ssh_transfer.rs`. |

## 3. Inventory: embedded (russh) path

| Concern                  | Value |
|--------------------------|-------|
| Default-build status     | Feature-gated behind `embedded-ssh` on `rsync_io` and `core`. Not in the workspace `default` set. Release artifacts do not include it. |
| Routing trigger          | `ssh://[user@]host[:port]/path` URL operands only. `core::client::run::mod.rs:215-234` checks `is_ssh_url` and only dispatches to `run_embedded_ssh_transfer` when the operand starts with `ssh://` and `embedded-ssh` was compiled in. |
| Entry point              | `core::client::remote::embedded_ssh_transfer::run_embedded_ssh_transfer` -> `rsync_io::ssh::embedded::connect_and_exec`. |
| Process model            | In-process tokio task driving `russh::client` over a TCP socket. No `fork`, no `exec`, no pipes. |
| ssh_config support       | Subset only (`crates/rsync_io/src/ssh/embedded/ssh_config.rs:1-100`): `Host`, `Hostname`, `User`, `Port`, `IdentityFile`, `IdentitiesOnly`, `IdentityAgent`. Wildcards in `Host` patterns are honoured with first-match-wins precedence. |
| `ProxyCommand`           | **Not supported.** No occurrences in `crates/rsync_io/src/ssh/embedded/`. |
| `ControlMaster` etc.     | **Not supported.** No occurrences in `crates/rsync_io/src/ssh/embedded/`. |
| `ProxyJump`              | **Not supported.** |
| `Match` / `Include`      | **Not supported.** The parser does not recognise the directives. |
| `ssh-agent` / hardware tokens | Agent forwarding via `IdentityAgent`; native agent socket protocol via `russh::keys::agent`. Hardware tokens require an agent that proxies them. |
| Goodbye-phase deadlock   | Eliminated by `SSH_GOODBYE_TIMEOUT` (SSR-4) plus russh's channel-based EOF model. |
| Test coverage            | 153 tests in the embedded path: 55 in `embedded/config.rs`, 27 in `embedded/auth.rs`, 22 in `embedded/resolve.rs`, 14 in `embedded/cipher.rs`, 12 in `embedded/ssh_config.rs`, 9 in `embedded/handler.rs`, 7 in `embedded/connect.rs`, 7 in `embedded/sync_bridge.rs`, plus 24 in `core/src/client/remote/embedded_ssh_transfer.rs`. |

## 4. Operator-config feature gap

| OpenSSH directive        | Subprocess path | Embedded path |
|--------------------------|-----------------|---------------|
| `Hostname`               | Yes (via `ssh`) | Yes |
| `User`                   | Yes             | Yes |
| `Port`                   | Yes             | Yes |
| `IdentityFile`           | Yes             | Yes |
| `IdentitiesOnly`         | Yes             | Yes |
| `IdentityAgent`          | Yes             | Yes |
| `StrictHostKeyChecking`  | Yes             | Yes (`StrictHostKeyChecking` enum) |
| `UserKnownHostsFile`     | Yes             | Yes |
| `Compression`            | Yes (detected by `config_lookup` to warn on double-compression) | No (russh selects cipher; rsync wire-codec only) |
| `ControlMaster` / `ControlPath` / `ControlPersist` | Yes | **No** |
| `ProxyCommand`           | Yes             | **No** |
| `ProxyJump` (`-J`)       | Yes             | **No** (builder exposes `-J` for the subprocess path only) |
| `Match` blocks           | Yes             | **No** |
| `Include`                | Yes             | **No** |
| `GSSAPIAuthentication`   | Yes             | **No** |
| `PreferredAuthentications` | Yes           | Hard-coded ordering: pubkey, agent, password |
| `KexAlgorithms` / `Ciphers` / `MACs` | Yes  | Fixed cipher list with AES-GCM preference |

The four gaps that actually matter to operators in production today
are `ControlMaster`, `ProxyCommand`, `ProxyJump`, and `Match`. They
cover jump-host topologies, per-host overrides, multiplexed
connection reuse, and bastion-via-corp-VPN flows. None of these are
plausible to reproduce inside russh in the SSR-2 timeframe.

## 5. Recommendation: KEEP

The subprocess SSH path stays in the tree, stays the default, and
stays under active maintenance. Justification:

1. **The default build is the subprocess build.** `embedded-ssh` is
   opt-in on the `rsync_io` crate, not in the workspace `default`
   feature set, and not enabled in `release-cross.yml`. Every
   binary downloaded from a release tag today routes `host:path` and
   `user@host:path` operands through `Command::new("ssh")`. Removing
   the subprocess path would break every existing operator install,
   not just the niche cases.
2. **`ssh://` URL routing is not a drop-in substitute for `host:path`.**
   The router only dispatches `ssh://` operands to russh
   (`run/mod.rs:215-234`). Operators using `git`-style `user@host:path`
   - which is the syntax in every rsync manpage example - never reach
   the embedded path even when `embedded-ssh` is compiled in.
   Re-routing `host:path` to russh would silently drop `ProxyCommand`,
   `ControlMaster`, `ProxyJump`, and `Match` support for those users.
3. **The post-PR-#4154 path is correct and benchmarked.** Goodbye
   deadlock is fixed; SSR-4's `SSH_GOODBYE_TIMEOUT` is the
   defence-in-depth backstop; the 3-way benchmark in
   `docs/benchmarks/ssh-transport-3way.md` tracks the
   subprocess-vs-russh gap release over release. The subprocess path
   is ~1.3x upstream on the v0.6.2 workload - slower than russh, but
   no longer a hang.
4. **The embedded path cannot reach feature parity in the SSR
   timeframe.** Implementing `ProxyCommand`, `ControlMaster`,
   `ProxyJump`, `Match`, `Include`, and `GSSAPIAuthentication` inside
   the russh client is a multi-quarter project. Deprecating the
   subprocess path before that work lands would strand the operators
   listed in section 4 with no working transport.
5. **Code-only deprecation would lie to operators.** A
   `#[deprecated]` attribute on `SshCommand` is invisible at the CLI
   layer where operators interact with the binary, and would not
   change which path executes. The honest signal is the SSR-3
   decision matrix and the SSR-4 deadlock guard, both of which are
   already in place.

## 6. What must happen next (non-blocking)

- **Track the russh feature gaps as named tasks.** `ProxyCommand`,
  `ControlMaster`, `ProxyJump`, and `Match` each warrant their own
  design doc before any code lands; until then the SSR-3 matrix is
  the source of truth on which path operators should pick.
- **Keep the 3-way benchmark in CI.** `benchmark.yml` already builds
  with `--features embedded-ssh` and runs the 3-way comparison so the
  subprocess-vs-russh gap stays visible.
- **Audit the subprocess path on every change.** Any new optimisation
  on the subprocess path must keep the stderr drain alive for the
  full transfer (the v0.6.1 lesson) and must not regress the
  benchmark gap below the SSR-3 matrix's published numbers.
- **Revisit this decision after one of two triggers fires:**
  (a) the embedded path covers `ProxyCommand` + `ControlMaster` +
  `ProxyJump`, or (b) a future regression demonstrates that the
  subprocess path costs more to maintain than the operator
  compatibility it preserves. Neither trigger fires today.

## 7. References

- SSR-3 policy matrix: [`docs/ssh-transport-decision-matrix.md`](../ssh-transport-decision-matrix.md)
- SSR-4 goodbye timeout: `crates/rsync_io/src/ssh/embedded/connect.rs:34` (`SSH_GOODBYE_TIMEOUT`)
- v0.6.1 regression post-mortem: [`docs/audits/ssh-daemon-perf-verification.md`](ssh-daemon-perf-verification.md)
- `ssh_config` parser scope: [`docs/design/ssh-config-parser-evaluation.md`](../design/ssh-config-parser-evaluation.md)
- 3-way benchmark methodology: [`docs/benchmarks/ssh-transport-3way.md`](../benchmarks/ssh-transport-3way.md)
- Subprocess vs russh stderr handling: [`docs/audits/ssh-socketpair-vs-pipes.md`](ssh-socketpair-vs-pipes.md), [`docs/audits/ssh-stderr-handling.md`](ssh-stderr-handling.md)
- Cipher / compression policy: [`docs/audits/ssh-cipher-compression.md`](ssh-cipher-compression.md)
