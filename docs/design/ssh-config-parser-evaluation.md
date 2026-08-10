# ssh_config parser evaluation (SSC-3)

## Recommendation

Adopt **`ssh2-config 0.7.x`** (MIT) as the ssh_config parser, behind a
`ssh-config-parse` cargo feature that is **enabled by default**. Wire the
parsed `Compression` directive into `SshCommand::has_ssh_compression()` so
the SSC-1 startup warning fires when SSH compression is enabled via
`~/.ssh/config` or `/etc/ssh/ssh_config` as well as via argv.

Why not the alternatives:

- **`russh-config`** does not recognise `Compression` at all (only
  `Host`, `User`, `HostName`, `Port`, `IdentityFile`, `ProxyCommand`,
  `ProxyJump`, `AddKeysToAgent`, `UserKnownHostsFile`, `StrictHostKeyChecking`).
  It is also tokio-mandatory through its workspace dependencies (`tokio`
  + `futures`), which conflicts with the no-async/threaded-only policy.
- **`ssh_cfg`** has not had a release since December 2021 and is async
  (`async fn parse`). Maintenance is dormant; deal-breaker.
- **Hand-rolled parser** is viable but duplicates well-tested upstream
  behaviour (boolean parsing, `Include` glob expansion, `IgnoreUnknown`,
  case-folding, line continuation, quoting). `ssh2-config` already
  handles those plus a usable `HostParams::compression: Option<bool>`.
  Roll our own only if `ssh2-config` introduces blocking issues.

Cost: 3 new transitive crates (`dirs`, `glob`, `wildmatch`); the other
three (`bitflags`, `log`, `thiserror`) are already in our `Cargo.lock`.
`ssh2-config`'s MSRV is 1.88.0, exactly matching `rust-toolchain.toml`.

## Candidate survey

| Crate | Latest | Released | License | Direct deps | Sync? | `Compression` | `Host` | `Match` | `Include` | `~` / glob paths | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `ssh2-config` | 0.7.1 | 2026-04-26 | MIT | 6 (`bitflags`, `dirs`, `glob`, `log`, `thiserror`, `wildmatch`) | yes | yes (`HostParams::compression: Option<bool>`) | yes | no | yes (recursive, glob) | yes (`~` + relative/absolute) | MSRV 1.88.0 = our pin; active; documented "missing features" list includes `Match`. |
| `russh-config` | 0.58.0 | 2026-03-18 | Apache-2.0 | `tokio` (io-util, net, macros, process), `futures`, `globset`, `whoami`, `log`, `thiserror` | parser sync; crate pulls async runtime | **no** | yes | no | no | partial | We already depend on `russh 0.60.3` transitively; `russh-config` is a separate sibling crate and not in our lock. Mandatory tokio dep is the deal-breaker for our threaded-only stance. |
| `ssh_cfg` | 0.3.0 | 2021-12-02 | MIT/Apache-2.0 | `async-std`, `plain_path`, `thiserror` | async-only API | unknown (un-audited; abandoned) | yes | no | no | yes | 4+ years dormant; do not adopt. |
| `openssh-config` | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | Not published on crates.io. |
| Hand-rolled subset | n/a | n/a | (ours) | 0 | yes | yes (we implement it) | yes | optional | optional | we implement | ~300-500 lines of parser + tests. Acceptable fallback; pay the maintenance cost only if `ssh2-config` blocks. |

### Dependency-footprint delta (recommended option)

`ssh2-config` itself + 3 new transitive crates (`dirs`, `glob`,
`wildmatch`). `dirs` brings `dirs-sys` and the platform shims, but all
three are widely vendored single-purpose crates (`dirs` >35M monthly
downloads; `glob` is a `rust-lang/glob` crate; `wildmatch` is a single
file). No new C linkage, no new async runtime, no new build scripts of
significance.

### License compatibility

We publish under Apache-2.0 OR MIT (`workspace.package.license`). MIT
(`ssh2-config`) and Apache-2.0 (`russh-config`) are both compatible with
our dual licence. No copyleft surfaces in the candidate set.

## Integration sketch

### Current call graph (post-SSC-1)

- `crates/rsync_io/src/ssh/builder.rs:236` defines
  `SshCommand::has_ssh_compression()`, which today only walks
  `self.options` for `-C` / `-o Compression=<truthy>` via
  `arg_enables_ssh_compression()` (builder.rs:639).
- `crates/core/src/client/remote/ssh_transfer.rs:296` invokes
  `warn_double_compression_once(config.compress(), ssh.has_ssh_compression())`
  just before `ssh.spawn()`.

The argv-only path stays as-is and remains the fast precedence check.
The new ssh_config probe is consulted only when the argv check returns
`false`, so we never re-walk config files on the hot path when the user
already settled the question on the command line.

### New surface in `rsync_io::ssh`

Add a sibling helper module `crates/rsync_io/src/ssh/config_probe.rs`
(no unsafe; `rsync_io` is on the no-unsafe list). It exposes:

```rust
pub fn ssh_config_enables_compression(
    host: &str,
    user_config: Option<&Path>,   // -F / ssh -F override; None = default
    system_config: Option<&Path>, // None = /etc/ssh/ssh_config
) -> bool;
```

Implementation outline:

1. Build the candidate file list in OpenSSH precedence order:
   - explicit `-F <file>` override, **else** `~/.ssh/config`
   - then `/etc/ssh/ssh_config`
   - (skip silently if the path does not exist or is unreadable -
     OpenSSH itself tolerates this; do **not** fail the transfer)
2. Parse each file with `ssh2_config::SshConfig::default().parse(...)`,
   then resolve `params(host)` against it. First-match-wins semantics
   match OpenSSH; `ssh2-config` already implements them.
3. Return `params.compression == Some(true)`. Treat `Some(false)` and
   `None` as "not set" (we only need a positive signal to warn).
4. On parse error: log at `debug!` level and return `false`. Never
   propagate to the caller. Matches OpenSSH's permissive posture and
   keeps the warning advisory.

`Match` blocks are deliberately **out of scope** for the first cut.
`Compression` is almost always set at top level or under `Host *`, both
of which `ssh2-config` handles. Document the limitation in
`oc-rsync(1)`; if real reports show Match-gated compression in the
wild, revisit. `Include` is in scope because `ssh-config-include`
patterns (e.g. `~/.ssh/config.d/*`) are common with `Compression`
nested inside.

### Wiring `SshCommand::has_ssh_compression`

Extend the existing method (no API churn for callers):

```rust
pub fn has_ssh_compression(&self) -> bool {
    if self.argv_enables_compression() {
        return true;
    }
    let Some(host) = self.parsed_host_for_config_probe() else {
        return false;
    };
    ssh_config_enables_compression(
        host,
        self.config_file_override(), // tracks -F / -oUserConfigFile= from argv
        None,                        // system path resolved inside probe
    )
}
```

- `argv_enables_compression()` is today's body, renamed.
- `config_file_override()` is a new helper that scans `self.options`
  for `-F <file>` / `-oUserConfigFile=<file>` to honour an explicit
  ssh override the user passed via `-e`. Mirrors OpenSSH precedence.
- `parsed_host_for_config_probe()` returns the destination host
  stripped of any `user@` prefix and `[]` brackets; on `None` (no
  destination yet, e.g. local-only build) the probe is skipped.

### Feature gate

Gate the new dep + module behind a default-on cargo feature
`ssh-config-parse` on `rsync_io`:

```toml
[features]
default = ["ssh-config-parse"]
ssh-config-parse = ["dep:ssh2-config"]
```

Stub `ssh_config_enables_compression` returns `false` when the feature
is off, so embedders who want the minimum footprint (no `dirs`/`glob`)
can opt out cleanly. The CLI feature surface in `crates/cli/Cargo.toml`
propagates the feature through the existing dep entry.

### Concurrency / threading

The probe runs once per `SshCommand::has_ssh_compression()` call from
`warn_double_compression_once()`. That call is made once per transfer
just before `ssh.spawn()`. Cost is two file reads at most plus a small
parse. No lock contention, no syscalls on the hot path, no need to
cache - the function is already gated by the `Once` inside
`warn_double_compression_once_*`.

### Cross-platform

On Windows, `~/.ssh/config` lives under `%USERPROFILE%\.ssh\config`
(`dirs::home_dir()` handles that) and `/etc/ssh/ssh_config` is absent.
The probe must:

- On non-Unix, skip the system path entirely.
- Still honour an explicit `-F <path>` override, which a user might
  point at a vendored Windows OpenSSH config.

No `#[cfg(unix)]` gating of the public API; the probe just returns
`false` early on Windows when no file exists. This keeps the warning
useful for the OpenSSH-for-Windows population without inventing
platform-specific call sites.

## Out of scope (explicit)

- Acting on the parsed value. SSC-3 only feeds the existing warning;
  it does not auto-disable `-z`, edit SSH argv, or refuse to spawn.
  Behaviour change is a separate decision, tracked separately if/when
  needed.
- `Match` block evaluation. First cut handles top-level and `Host`
  patterns only.
- Cipher, MAC, KEX, or any non-`Compression` directives. The probe
  surface is intentionally narrow to one boolean.

## Follow-up tasks (SSC-3 punch list)

- **SSC-3.a Implementation PR.** Add `rsync_io/src/ssh/config_probe.rs`,
  the `ssh-config-parse` feature, `ssh2-config` dep, and the
  `SshCommand::has_ssh_compression()` extension described above. No
  changes to `warn_double_compression_once_*` semantics.
- **SSC-3.b Unit tests against synthetic fixtures.** Cover:
  top-level `Compression yes`; `Host *` block; matching `Host pattern`;
  non-matching host falls through; `Compression no` does **not** trip
  the warning; `Compression yes` under `Include` directive; missing
  file is silently tolerated; malformed file logs and returns false.
- **SSC-3.c Fixture tests.** Drop sample configs under
  `crates/rsync_io/tests/fixtures/ssh_config/` and exercise them via a
  new integration test `crates/rsync_io/tests/ssh_config_probe.rs`,
  mirroring the layout of `parse_remote_shell.rs` /
  `ssh_stderr_default_path.rs`. Include at minimum: `simple_top.conf`,
  `host_star.conf`, `include_glob.conf` plus `included/*.conf`,
  `malformed.conf`, and `compression_off.conf`.
- **SSC-3.d Docs.** Update the README "Avoid SSH + rsync
  double-compression" section to drop the "does **not** parse
  `~/.ssh/config`" caveat and document the new probe (including its
  `Match`-block limitation and the `-F` override behaviour). Update
  `man/oc-rsync.1` if it carries the same caveat.
- **SSC-3.e Close the memory notes.** Mark
  `feedback_ssh_compression_no_config_parse.md` and
  `project_ssh_compression_no_config_parse.md` as RESOLVED once SSC-3.a
  through SSC-3.d land. Cross-link from the SSC series in AGENTS.md.
- **SSC-3.f Optional fuzz target.** Lightweight `cargo-fuzz` harness on
  `ssh_config_enables_compression(host, fixture, None)` reading from a
  small corpus of real-world configs (sanitised). Catches panics in
  `ssh2-config` we'd inherit. Defer to FCV series cadence.
