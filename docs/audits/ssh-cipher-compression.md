# SSH cipher-level compression detection

Tracking issue: oc-rsync task #2046.

## Summary

When the user runs oc-rsync over SSH and the SSH client is configured to
compress its data stream (`-C` on the argv, or `Compression yes` in
`~/.ssh/config` for the target host), enabling rsync's own `--compress` on
top is wasteful: zlib/zstd is applied first by rsync, then SSH's compression
spends CPU re-compressing already-compressed data and saves nothing. On
modern links the double-compression is consistently slower than either layer
alone.

Upstream rsync 3.4.1 does not detect this collision. The user is expected to
know which side compresses and pick one. oc-rsync can do better at zero
wire-protocol cost: detection is a local, pre-spawn check on the SSH argv
and (optionally) the resolved `ssh_config` for the host.

This audit catalogs the detection signals we have at command-build time,
proposes a user surface, and phases the work so that Phase 1 ships only
docs (this file). No Rust, no Cargo.toml, no flag wiring is changed by
landing this audit.

This is a quality-of-life feature, not a wire-protocol or correctness
concern. Both rsync `--compress` and SSH compression remain individually
correct; the only impact of double-compression is wasted CPU and slightly
worse throughput.

## Current oc-rsync surface

The SSH transport lives in `crates/rsync_io/src/ssh/`:

- `crates/rsync_io/src/ssh/mod.rs` re-exports `SshCommand`, `SshConnection`,
  `parse_ssh_operand`, and `parse_remote_shell` as the public surface.
- `crates/rsync_io/src/ssh/builder.rs` defines `SshCommand` (the builder)
  and renders the argv via `SshCommand::command_parts`. Existing pre-spawn
  guards mirror the pattern this audit recommends:
  - `should_inject_aes_gcm_ciphers` and `options_contain_cipher_flag` decide
    whether to inject `-c aes...` based on the user's existing options.
  - `should_inject_keepalive` and `options_contain_keepalive` skip
    `ServerAliveInterval` injection when the user already specified it.
  - `should_inject_connect_timeout` and `options_contain_connect_timeout`
    skip `ConnectTimeout` injection in the same way.
  - `is_ssh_program` decides whether the configured remote shell is an
    OpenSSH client at all (`ssh` / `ssh.exe`, case-insensitive on Windows).
- `crates/rsync_io/src/ssh/parse.rs` exposes `parse_remote_shell`, which
  tokenises `RSYNC_RSH` / `--rsh` strings via `shell-words` so we can
  inspect the resulting argv.
- `crates/rsync_io/src/ssh/operand.rs` exposes `parse_ssh_operand`, which
  splits `user@host:path` and `[ipv6]:path` into structured components.
- `crates/rsync_io/src/ssh/embedded/` holds the embedded-SSH path used when
  the system `ssh` binary is unavailable. It does not negotiate compression
  today; it is out of scope for this audit.

The CLI `--compress` surface lives in
`crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs`:

- `--compress` / `-z` is defined at line 56 (`Arg::new("compress")`).
- `--no-compress` / `--no-z` is defined at line 64.
- `--compress-level` / `--zl`, `--compress-choice` / `--zc`, `--old-compress`,
  `--new-compress`, `--skip-compress` follow at lines 72, 81, 91, 98, 105.

The legacy `crates/cli/src/frontend/command_builder/sections/build_base_command/`
tree (`core_args.rs`, `network.rs`, `transfer.rs`, etc.) does not currently
host `--compress`. Compression flags live in
`connection_and_logging_options.rs` alongside other transport-affecting
options. Adding a sibling flag (Phase 2 or 3) would slot in next to the
existing `Arg::new("compress")` block.

The compression configuration accessors are on `ClientConfig` in
`crates/core/src/client/config/client/performance.rs`: `compress()`,
`compression_level()`, `compression_algorithm()`,
`explicit_compress_choice()`, `compression_setting()`, `skip_compress()`.
A double-compression detector would consume `compress()` plus the SSH argv
that `build_ssh_connection` is about to spawn.

The SSH transfer entry point is `build_ssh_connection` in
`crates/core/src/client/remote/ssh_transfer.rs:257`. It receives a
`ClientConfig` and the parsed remote operand, then walks through the
`SshCommand` builder calls (`set_user`, `set_port`, `set_program`,
`push_option`, `set_bind_address`, `set_prefer_aes_gcm`,
`set_jump_hosts`, `set_connect_timeout`, `set_remote_command`, `spawn`).
This is the natural integration point for a pre-spawn detection hook.

## Detection signals available at command-build time

The detector runs before `SshCommand::spawn`. By that point we know the
program (e.g. `ssh`, `plink`), the user-supplied options (from `--rsh`
or `RSYNC_RSH`), the host string, the port, and whether `ClientConfig`
requested `--compress`. Three signals are visible:

### 1. `-C` or `Compression=yes` in argv (definitive)

Source: `RSYNC_RSH` / `--rsh` parsed by `parse_remote_shell`, plus any
`SshCommand::push_option` call. After
`SshCommand::configure_remote_shell` the values land in the
`SshCommand::options` vector. A scan is straightforward:

- A standalone `-C` token (separate argv element).
- A combined token starting with `-C` that does not look like a different
  flag (OpenSSH only documents bare `-C`; combined forms like `-Caes...`
  do not exist for SSH compression specifically).
- `-o Compression=yes` as a split pair (`-o`, `Compression=yes`).
- `-oCompression=yes` as a single concatenated token.
- `-o`, `Compression yes` as a split pair (OpenSSH accepts both `=` and
  whitespace separators in `-o key value`).

Recommended classifier: case-insensitive comparison on the key
(`Compression`) and value (`yes`, `true`, `1`). OpenSSH `ssh_config(5)` lists
`yes` and `no` as the only valid values, but rejecting `force` etc. is fine
(unknown values mean SSH would refuse to start anyway).

This signal is definitive because the user has explicitly opted in and the
classifier sees the exact argv we are about to spawn.

### 2. `ssh_config` resolved via `ssh -G HOST` (probable)

Source: parsed config files, host-specific `Match` blocks, and `Include`
directives. The robust way to read the resolved configuration is to invoke
`ssh -G HOST`, which prints the merged configuration in `key value\n` form
without performing a connection. We can run this once, when both
`config.compress()` is true AND the SSH transport is in use, and grep the
output for a leading `compression yes` line (case-insensitive key, since
OpenSSH lower-cases keys in `-G` output).

Caveats:

- `ssh -G` is OpenSSH-specific. `plink`, `dropbear`, `rsh`, and the embedded
  client do not implement it. The detector must skip cleanly when the
  configured program is not an OpenSSH client. The `is_ssh_program` predicate
  in `crates/rsync_io/src/ssh/builder.rs` already performs this check.
- `ssh -G` runs the local SSH config parser. On unusual setups this can be
  slow (tens of milliseconds on large configs with many `Match exec` blocks
  that fork shell scripts). Run it at most once per session; cache by
  resolved `(user, host, port)` triple. Treat any non-zero exit as "no
  signal" rather than an error.
- `ssh -G` does NOT reflect future runtime overrides in `~/.ssh/config`
  changed mid-session, which is fine: we only care about the spawn we are
  about to make.
- The resolved value reflects the host alias the user passed, not the final
  hostname. That is the right granularity: `Match host X` blocks attach to
  the alias.

This signal is "probable" rather than "definitive" because the user can
override per-invocation with `-o Compression=no`, which appears later in the
SSH argv and wins. The detector should preserve precedence: if signal 1
shows `-o Compression=no` we suppress the warning even if signal 2 says
`compression yes`.

### 3. Cipher name implying inline compression (none in modern OpenSSH)

OpenSSH historically supported `arcfour` and `blowfish-cbc`; neither
performs compression. No cipher in any current OpenSSH release implies
compression. We flag this branch only to document that the matrix has been
considered. No detection action is required.

## Recommended detection logic

Pseudo-flow, executed in `build_ssh_connection` after the `SshCommand`
builder has been fully populated and immediately before the `ssh.spawn()`
call at `crates/core/src/client/remote/ssh_transfer.rs:306`:

```text
if !config.compress() { return None; }
if !ssh.is_ssh_program() { return None; }

// Signal 1: scan argv we built.
let argv = ssh.options();      // or a new accessor on SshCommand.
let argv_signal = scan_argv_for_compression(argv);
if argv_signal == Suppressed { return None; }   // -o Compression=no

// Signal 2: ask ssh -G if signal 1 was inconclusive.
let cfg_signal = match argv_signal {
    Enabled => Enabled,
    Inconclusive => probe_ssh_config(program, user, host, port),
};

if cfg_signal == Enabled {
    Some(DoubleCompressionWarning { reason: argv_or_config })
} else {
    None
}
```

The `probe_ssh_config` helper runs `ssh -G HOST` with a short timeout
(suggest 250 ms; the work is read-only), captures stdout, and returns
`Enabled` only on `compression yes`. Errors map to `Inconclusive`.
Cache the result keyed by the resolved program, user, host, and port for
the lifetime of the process.

## User surface

Two sub-options were considered:

### (a) `--compress-warn-double` (Phase 2, recommended on by default)

When detection fires, write a single line to stderr before `spawn()`:

```text
oc-rsync: ssh appears to compress this connection; --compress is redundant
```

Rationale: the user is doing something benign but suboptimal, and an
informative one-liner is the lightest-touch correction. The flag exists
mainly as an off switch for users in pipelines who do not want the diagnostic.

This matches the precedent set by upstream rsync's
`If you specify --compress, the data is also compressed.` style of
informational message and avoids changing transfer semantics. It also
mirrors how oc-rsync already injects `-oServerAliveInterval=20` silently
but inverts the polarity (warn rather than auto-act).

### (b) `--compress-on-double=auto|force|off` (Phase 3, deferred)

- `auto` (the default once enabled): silently disable rsync `--compress`
  when SSH compression is detected. Equivalent to inserting
  `set_compress(false)` on the `ClientConfig` after detection.
- `force`: keep `--compress` even when detection fires (today's behaviour).
- `off`: disable compression unconditionally.

Defer (b) until we have benchmarks showing the win is large enough to
justify changing user-visible transfer behaviour. The risk is silently
disabling rsync compression for a user who relied on its skip-compress
list (`--skip-compress=zip,gz,...`) to differ from SSH's internal
heuristics; SSH compression treats every byte uniformly.

## Open questions

1. **Daemon-via-SSH (`-rsh ssh ... rsync://host/module/path`).** This path
   uses SSH only as a tunnel for the daemon protocol, but the wire bytes
   between oc-rsync and the daemon still cross the SSH stream and would
   still be double-compressed if both layers compress. The detector applies
   identically; no special-case is needed. Pure `rsync://...` (TCP, no SSH)
   is unaffected and the detector must not run.
2. **`ssh -G` unavailable or slow.** The detector falls back to argv-only
   detection (signal 1). On the slow path, cache the result for the process
   lifetime so a multi-host transfer running against many remotes does not
   pay the parsing cost on each connection.
3. **Non-OpenSSH clients (`plink`, `rsh`, embedded).** Skip detection
   entirely. `is_ssh_program` already gates this. The embedded SSH client
   in `crates/rsync_io/src/ssh/embedded/` does not currently negotiate
   compression at all (a search for `compression`/`Compression` returns no
   matches in that subtree), so there is nothing to detect.
4. **`-e` overriding `RSYNC_RSH`.** Both end up in `SshCommand::options`
   via `configure_remote_shell` or `push_option`. The detector inspects the
   final argv, so precedence is naturally handled.
5. **Cipher-driven compression.** No modern cipher implies compression.
   Document the audit and move on; no code branch is needed.
6. **Multiple hosts in one invocation (`--files-from` with mixed
   `host:path` entries, or `host1:src host2:dst` rejected by upstream).**
   The detector keys its cache on `(program, user, host, port)`. Each
   spawn pays its own probe; the cost is bounded by the number of distinct
   hosts.
7. **Nested `ProxyJump`.** `-J host1,host2` may pull `Compression yes` from
   any hop's config. `ssh -G HOST` already merges across `Match` blocks for
   the final destination but does not enumerate intermediate hops. We
   accept the limitation: detect the destination hop only.

## Cross-references

- Task #1818: scope of the `tokio` dependency. Relevant because the
  `ssh -G` probe is a synchronous subprocess and should stay synchronous to
  avoid pulling additional async surface into `rsync_io`. No interaction
  with `tokio` is needed for Phase 2.
- Tasks #1593 and #1411: async SSH evaluation. The detector is independent
  of whether the SSH transport is sync (today) or async (potential future).
  It runs before `spawn()` and writes to stderr; both modes can call it
  unchanged.
- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/io.c`
  and `clientserver.c`. Compression negotiation in upstream rsync happens
  inside the rsync protocol on the wire (`io.c` tag bytes for the
  `MSG_DEFLATED` envelope; `clientserver.c` for daemon-side option parsing).
  Neither file consults the SSH transport; upstream therefore has no notion
  of double-compression and never warns. This audit is purely an oc-rsync
  enhancement at the transport layer and does not touch the wire.

## Phasing

### Phase 1 (this audit, docs only)

- Land this file. No code changes.
- No new flags, no detection code, no behaviour change.

### Phase 2 (passive detection, opt-out warning)

Deliverables:

- New `is_compression_enabled` predicate plus `probe_ssh_config` helper in
  a new submodule under `crates/rsync_io/src/ssh/` (suggest
  `compression_detect.rs`). Both surface as private helpers consumed only
  via a small public `detect_double_compression(&SshCommand) -> Option<Reason>`
  call.
- Add an `options()` accessor to `SshCommand` so the detector can scan the
  final argv without duplicating builder state. Today only `command_parts`
  observes the argv, and it is private.
- Wire the detector into `build_ssh_connection` in
  `crates/core/src/client/remote/ssh_transfer.rs` between
  `set_remote_command` and `spawn`.
- Add `--compress-warn-double` (default on) and `--no-compress-warn-double`
  in
  `crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs`,
  plumbed through `ClientConfig` accessors so the warning honours the user
  preference.
- Tests: pure-argv cases (no `ssh -G` invocation) covering the four
  argv forms (`-C`, `-oCompression=yes`, `-o Compression=yes`, mixed-case),
  the `Compression=no` override case, and the non-OpenSSH program case.
  The `ssh -G` integration test is gated behind `cfg(unix)` and skipped
  when `ssh` is not on `PATH`.

Acceptance criteria:

- Warning fires exactly once per process per `(user, host, port)` triple.
- Warning suppressed when `--no-compress-warn-double` is set, when
  `--no-compress` is set, or when the configured program is not OpenSSH.
- No measurable regression in SSH spawn latency when `--compress` is not
  requested (the detector exits at the first guard).

### Phase 3 (opt-in auto-disable)

Deliverables:

- `--compress-on-double=auto|force|off` flag, defaults to `force` (today's
  behaviour) until benchmarks justify flipping.
- When set to `auto`, mutate the `ClientConfig` to clear `compress` after
  detection fires, and emit the warning in past tense
  (`...; disabling --compress`).
- Benchmark harness extension: add a hyperfine row in
  `scripts/benchmark_remote.sh` that exercises double-compression vs.
  rsync-only vs. ssh-only for a 1 GiB random and a 1 GiB highly-compressible
  payload, so that the policy decision rests on data and not folklore.

Acceptance criteria:

- No silent change in transfer semantics when the flag is absent or set to
  `force`.
- When set to `auto`, the on-the-wire bytes match an explicit
  `--no-compress` invocation against the same source set.

## References

- SSH transport entry point: `crates/rsync_io/src/ssh/mod.rs` (re-exports
  at lines 84-87 of master).
- SSH builder and existing pre-spawn guards:
  `crates/rsync_io/src/ssh/builder.rs`
  (`SshCommand`, `should_inject_aes_gcm_ciphers`,
  `options_contain_cipher_flag`, `should_inject_keepalive`,
  `options_contain_keepalive`, `should_inject_connect_timeout`,
  `options_contain_connect_timeout`, `is_ssh_program`,
  `command_parts`).
- SSH argv tokeniser: `crates/rsync_io/src/ssh/parse.rs`
  (`parse_remote_shell`, `RemoteShellParseError`).
- Remote operand split: `crates/rsync_io/src/ssh/operand.rs`
  (`RemoteOperand`, `parse_ssh_operand`).
- SSH transfer driver: `crates/core/src/client/remote/ssh_transfer.rs`
  (`build_ssh_connection`, lines 257-327).
- CLI `--compress` definitions:
  `crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs`
  (lines 56-112).
- Compression accessors:
  `crates/core/src/client/config/client/performance.rs`
  (`compress`, `compression_level`, `compression_algorithm`,
  `explicit_compress_choice`, `compression_setting`, `skip_compress`).
- Upstream rsync 3.4.1 source (informational, no behaviour borrowed):
  `target/interop/upstream-src/rsync-3.4.1/io.c` (multiplex envelope,
  `MSG_DEFLATED` tag) and `clientserver.c` (daemon option parsing). Neither
  file consults SSH; upstream does not warn on double-compression.
- OpenSSH manual pages: `ssh(1)` (`-C`, `-G`), `ssh_config(5)`
  (`Compression`).

Last verified: 2026-05-01 against master at commit 83c8aa41
("docs(fast_io): basis-file I/O policy: mmap vs buffered with io_uring
(#1666)"). Spot-checked files:
`crates/rsync_io/src/ssh/mod.rs`,
`crates/rsync_io/src/ssh/builder.rs`,
`crates/rsync_io/src/ssh/parse.rs`,
`crates/rsync_io/src/ssh/operand.rs`,
`crates/core/src/client/remote/ssh_transfer.rs`,
`crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs`,
`crates/core/src/client/config/client/performance.rs`.
