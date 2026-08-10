# SSC-4.a: ssh_config `Match` conditions to honor in `has_ssh_compression()`

## Summary

SSC-3.impl extended `SshCommand::has_ssh_compression()` to consult
`~/.ssh/config` and `/etc/ssh/ssh_config` via a hand-rolled parser in
`crates/rsync_io/src/ssh/config_lookup.rs`. That parser honours only
top-level directives and `Host *` blocks; everything inside a `Match`
block is treated as a different active block and skipped (see
`Block::Match` arm of `parse_enables_compression`). SSC-4 closes that
gap. This document fixes the contract for SSC-4.b implementation: which
`Match` conditions we evaluate, which we skip, how negation and pattern
lists are interpreted, the order-of-evaluation rule, and the test
fixtures required for SSC-4.d.

No source changes here. This is a spec.

## Background

`has_ssh_compression()` drives `warn_double_compression_once()` in
`crates/core/src/client/remote/ssh_transfer.rs`. It is an advisory
warning. False positives are nearly as bad as false negatives: the
warning loses meaning if it fires when SSH compression is not actually
on for *this* connection.

OpenSSH evaluates `Match` blocks during connection setup against the
runtime context of the impending connection: target host, resolved
hostname, remote user, local user, optional `-P` tag, and (for
`exec`) the output of a subprocess. We have most of that context at
the warning site:

| Context | Available in `SshCommand`? |
|---|---|
| target hostname (argv `host`) | yes (`self.host`) |
| remote user | yes (`self.user`, may be `None`) |
| local user | yes (`whoami` / `USER` / `USERNAME` env) |
| `-P` tag | parseable from `self.options` |
| canonicalized hostname | no (we do not run `CanonicalizeHostname`) |
| second-pass `Final` re-evaluation | no |

That asymmetry shapes the HONOR / SKIP / DEFER decisions below.

## The full `Match` vocabulary (`ssh_config(5)`)

OpenSSH defines nine conditions on a `Match` line. Each takes a
pattern-list argument except `all`, `canonical`, and `final`, which take
no argument.

| Condition | Argument | Meaning |
|---|---|---|
| `host` | pattern-list | matches the target hostname *after* any `Hostname` substitution and (when active) canonicalization |
| `originalhost` | pattern-list | matches the hostname exactly as given on the command line, before any substitution |
| `user` | pattern-list | matches the remote user (`-l`, `user@host`, or default) |
| `localuser` | pattern-list | matches the local user running `ssh` |
| `exec` | command | runs the command in the user's shell; matches when exit status is 0 |
| `all` | (none) | always matches; conventional terminator for a multi-condition list |
| `canonical` | (none) | matches only during the post-canonicalization second pass |
| `final` | (none) | matches only during the post-canonicalization, post-`Final` second pass; also triggers a second pass when no canonicalization is configured |
| `tagged` | pattern-list | matches when the active `-P` tag matches the pattern-list |

A single `Match` line may combine multiple conditions; all must match
for the block to apply (logical AND across the line). Within one
condition the pattern-list is a comma- or whitespace-separated set of
glob patterns, and `!pattern` negates a single token (see "Negation
handling" below).

## HONOR / SKIP / DEFER decisions

### HONOR (ship in SSC-4.b)

| Condition | Why honor | Implementation cost |
|---|---|---|
| `host` | Same data we already have. The warning site knows the target. | Reuse `pattern_matches` from `embedded/ssh_config.rs` (glob `*`/`?`). |
| `originalhost` | We never substitute `Hostname` before warning, so for our purposes `host` and `originalhost` are equivalent. Both evaluate against `self.host`. | Same matcher as `host`. |
| `user` | We know `self.user`. When `None`, treat as "no remote user specified" and let `*` patterns match (mirrors OpenSSH default-user semantics where `User` defaults to `$USER`). | Reuse `pattern_matches`. When `self.user` is `None`, fall back to local user. |
| `localuser` | Cheap; same env lookup OpenSSH uses (`getlogin()` / `pw_name`). Cross-platform via `USER` (Unix) or `USERNAME` (Windows). | New `local_user()` helper. |
| `all` | Argumentless, always true. Required so users can write `Match all` as a wildcard default block. | One-liner. |

### SKIP (intentionally not implemented)

| Condition | Why skip |
|---|---|
| `canonical` | Activates only during the post-canonicalization pass. We do not implement `CanonicalizeHostname`, so this pass never runs. A `Match canonical` block in the wild config is dead code from our perspective. Skipping is conservative: we never miss a *first-pass* `Compression yes` because of an unrelated `canonical` block. |
| `final` | Same reasoning. Triggers a second pass we do not perform. |
| `tagged` | OpenSSH's `-P tag` is a CLI flag we neither set nor surface. No oc-rsync code path produces a tag. Skipping cannot produce a false positive. If we ever expose tagged connections, revisit. |

A block whose `Match` line contains *any* of `canonical`, `final`, or
`tagged` is treated as non-matching for SSC-4.b. Even when other
conditions on the same line evaluate true, the unrecognised condition
on the AND-chain prevents the block from applying. This is the
conservative reading and matches what OpenSSH does during the first
pass when canonicalization is not configured.

### DEFER (out of scope for SSC-4.b; tracked for a later sprint)

| Condition | Why defer |
|---|---|
| `exec` | Requires `Command::new(shell).arg("-c").arg(cmd).status()` on the warning hot path. Three real costs: (1) fork/exec on every transfer is measurable; (2) executing arbitrary user-supplied shell snippets from an unprivileged read path inverts the trust model the warning was supposed to live inside; (3) reproducible behaviour requires us to replicate OpenSSH's exact shell selection (`SHELL` env, `/bin/sh` fallback, Windows `cmd /c`), which is a small surface area but a real one. None of these are blockers, but none belong in a one-shot advisory warning. Defer to a future SSC-5 if real reports show `Match exec`-gated `Compression yes` in the wild. The conservative outcome (skip the block, do not warn) is acceptable: SSC-1's argv path still catches `-C` and `-o Compression=yes`. |

Blocks whose `Match` line contains `exec` are treated as non-matching
for SSC-4.b, same rule as the SKIP set.

### Decision table

| Condition | Decision | First-pass eval data we hold | Risk if we got it wrong |
|---|---|---|---|
| `host` | HONOR | `self.host` | Low - false positive only if user wrote a Match block they did not intend |
| `originalhost` | HONOR | `self.host` (we never substitute) | Same |
| `user` | HONOR | `self.user` or local user fallback | Low |
| `localuser` | HONOR | `USER` / `USERNAME` env | Low |
| `all` | HONOR | n/a | None |
| `canonical` | SKIP | n/a | None - we never reach pass 2 |
| `final` | SKIP | n/a | None - we never reach pass 2 |
| `tagged` | SKIP | n/a | None - no tag is ever set |
| `exec` | DEFER | n/a (subprocess) | False negative only; argv path still catches `-C` |

**HONOR count: 5** (host, originalhost, user, localuser, all)

## Negation and pattern semantics

OpenSSH's pattern syntax is identical across `Host` blocks and `Match`
pattern-lists, and our embedded parser
(`crates/rsync_io/src/ssh/embedded/ssh_config.rs`) already implements
it via `pattern_matches` + `host_matches_any_pattern`. SSC-4.b reuses
that exact code path.

- **Pattern-list separator.** Tokens within a pattern-list are split on
  whitespace **or** commas. `Match host foo,bar baz` is three tokens.
- **Glob metacharacters.** `*` matches any run, `?` matches one
  character. No character classes (`[abc]`), no `**`, no extended
  globs - same as `fnmatch(3)` without `FNM_PATHNAME`.
- **Negation.** A leading `!` on a single token negates that token. As
  in OpenSSH and our `Host` matcher, *any* negated token that matches
  the input causes the whole pattern-list to fail to match, even if a
  positive token also matched. Pattern-lists thus express
  "match X but not Y" inline.
- **Empty pattern-list.** Treated as a no-op (block not applied). A
  warning at parse time is unnecessary - the parser already skips
  malformed lines silently with a single `debug_log!`.
- **Case folding.** Hostnames and usernames are compared
  case-insensitively for `host` / `originalhost`; usernames for
  `user` / `localuser` are compared case-sensitively on Unix and
  case-insensitively on Windows. OpenSSH's `match_pattern_list` lowers
  hostnames; we do the same. Local usernames on Windows already come
  back canonicalized by the OS.

## First-match-wins ordering

OpenSSH's rule is **first-match-wins per directive**, not per block:
every config file is scanned top to bottom for each option, and the
*first* assignment of `Compression` (whether in a `Host`, `Match`, or
top-level scope) is the one that sticks. Subsequent assignments are
ignored.

Our existing `parse_enables_compression` already encodes this correctly
for `TopLevel` and `HostStar`: each slot is `Option<bool>` and only set
when previously `None`. SSC-4.b extends the rule to a fourth slot for
matching `Match` blocks. Resolution order:

1. Walk the file once.
2. For each `Compression <value>` directive, find the *active* block
   (`TopLevel`, `Host *`, `Host <pattern>`, or `Match ...`).
3. If the active block is one we evaluate (top-level, `Host *`, or a
   `Match` block whose conditions all pass), and the corresponding slot
   is unset, set it.
4. After parsing, return `true` if **any** evaluated slot is `Some(true)`.

The "any slot true" final OR mirrors OpenSSH's "first assignment
wins" because we never overwrite a slot. Two `Compression` lines in
different applicable blocks will both attempt to set their respective
slot; the first one in each block lineage wins. The combined OR is
conservative: any path that would have caused OpenSSH to negotiate
compression flips the warning. The warning fires once per process via
`warn_double_compression_once`, so duplicate triggering is impossible.

## Edge cases for `has_ssh_compression` specifically

- **`Match all` followed by `Compression yes`.** Must trigger. This is
  the canonical "global default" pattern many users prefer over
  `Host *`.
- **`Match host *.example.com user deploy` with our target
  `web1.example.com` and `self.user = Some("deploy")`.** Must trigger.
  Both conditions on the AND-chain pass.
- **Same line, `self.user = Some("root")`.** Must not trigger. The
  `user` condition fails, so the block is inert.
- **`Match exec "test -f /tmp/foo"` with `Compression yes`.** Must
  not trigger (DEFER decision). The argv path is the only fallback,
  and that is acceptable.
- **`Match canonical` with `Compression yes`.** Must not trigger
  (SKIP decision).
- **Mixed `Match host foo canonical` with `Compression yes`.** Must
  not trigger - `canonical` is on the SKIP list, and the AND-chain
  fails as soon as any unrecognised condition is encountered.
- **Negation: `Match host !banned,*`.** Must trigger for any host
  other than `banned`. Reuses `host_matches_any_pattern`.
- **`Match host *` with no `Compression` directive inside.** No-op.
  Block is evaluated but contributes nothing.
- **`Compression no` inside a matching `Match` block, after a
  top-level `Compression yes`.** Top-level wins (set first). Returns
  `true`. Mirrors OpenSSH's first-match-wins.
- **`Compression yes` inside a matching `Match` block, after a
  top-level `Compression no`.** Top-level wins. Returns `false`. The
  conservative outcome: we do not warn, but `-z` is double-compressing
  against nothing because the user explicitly disabled SSH compression.

## Test fixtures (for SSC-4.d)

Drop these under `crates/rsync_io/tests/fixtures/ssh_config/match/`
and exercise each in a new integration test
`crates/rsync_io/tests/ssh_config_match.rs`. Each fixture's expected
behaviour is asserted via the public `SshCommand::has_ssh_compression`
contract.

| Fixture | Contents | Target host | Remote user | Expected |
|---|---|---|---|---|
| `match_all.conf` | `Match all`<br>`  Compression yes` | any | any | `true` |
| `match_host_glob.conf` | `Match host *.example.com`<br>`  Compression yes` | `web1.example.com` | any | `true` |
| `match_host_glob.conf` | (same) | `db.internal` | any | `false` |
| `match_host_negation.conf` | `Match host *.example.com,!banned.example.com`<br>`  Compression yes` | `ok.example.com` | any | `true` |
| `match_host_negation.conf` | (same) | `banned.example.com` | any | `false` |
| `match_originalhost.conf` | `Match originalhost web*`<br>`  Compression yes` | `web1` | any | `true` |
| `match_user.conf` | `Match user deploy`<br>`  Compression yes` | any | `deploy` | `true` |
| `match_user.conf` | (same) | any | `root` | `false` |
| `match_localuser.conf` | `Match localuser <env-user>`<br>`  Compression yes` | any | any | `true` (test must set `USER` / `USERNAME` to the literal) |
| `match_combined_and.conf` | `Match host web* user deploy`<br>`  Compression yes` | `web1` | `deploy` | `true` |
| `match_combined_and.conf` | (same) | `web1` | `root` | `false` |
| `match_canonical_skip.conf` | `Match canonical`<br>`  Compression yes` | any | any | `false` (SKIP) |
| `match_final_skip.conf` | `Match final`<br>`  Compression yes` | any | any | `false` (SKIP) |
| `match_tagged_skip.conf` | `Match tagged prod`<br>`  Compression yes` | any | any | `false` (SKIP) |
| `match_exec_deferred.conf` | `Match exec "true"`<br>`  Compression yes` | any | any | `false` (DEFER) |
| `match_mixed_unknown.conf` | `Match host web* canonical`<br>`  Compression yes` | `web1` | any | `false` (AND-chain breaks on `canonical`) |
| `match_top_level_wins.conf` | `Compression no`<br>`Match all`<br>`  Compression yes` | any | any | `false` (top-level first) |
| `match_first_match_in_block.conf` | `Match host *`<br>`  Compression yes`<br>`  Compression no` | any | any | `true` (first `Compression` in block wins) |
| `match_empty_pattern.conf` | `Match host`<br>`  Compression yes` | any | any | `false` (malformed line; block inert) |
| `match_no_compression_directive.conf` | `Match all`<br>`  ServerAliveInterval 60` | any | any | `false` |

Tests must isolate `HOME` and `USERPROFILE` via the existing
`EnvGuard` helper before pointing them at the fixture tree (the same
pattern used by `tests/ssh_config_compression.rs`).

## Connection to SSC-3.impl

The SSC-3 evaluation doc
(`docs/design/ssh-config-parser-evaluation.md`) recommended adopting
the external `ssh2-config` crate. The actual SSC-3.impl rolled a
hand-written parser in `crates/rsync_io/src/ssh/config_lookup.rs`
instead - the "Hand-rolled subset" alternative in that doc's
comparison table - and ships behind the default-on `ssh-config-parse`
feature with zero new dependencies. The same module gates the embedded
russh transport's per-host resolver
(`crates/rsync_io/src/ssh/embedded/ssh_config.rs`), which already
implements `Host` patterns with glob + negation via
`host_matches_any_pattern` and `pattern_matches`.

SSC-4.b therefore needs **no new external dependency**. The
implementation is purely additive on the existing parser:

1. Add a fifth `Block` variant `MatchEvaluated(bool)` that records
   whether the active `Match` line's AND-chain passed evaluation. The
   `Compression` arm consults this when assigning to a new
   `match_block: Option<bool>` slot, with the same "set once" rule as
   `top_level` and `host_star`.
2. Add an evaluator
   `fn match_line_applies(line: &str, ctx: &MatchContext) -> Decision`
   that returns one of `Applies`, `DoesNotApply`, or `Skip` (for
   `canonical` / `final` / `tagged` / `exec`). `Skip` is treated as
   `DoesNotApply` in the parser.
3. Plumb a `MatchContext { host, user, local_user }` from the caller.
   `host` comes from `SshCommand::host`, `user` from
   `SshCommand::user.or_else(local_user)`, `local_user` from the same
   env lookup OpenSSH uses (`USER` on Unix, `USERNAME` on Windows).
4. The public signature of
   `ssh_config_enables_compression(options: &[OsString])` widens to
   `ssh_config_enables_compression(options: &[OsString], ctx:
   &MatchContext)`, with a private convenience wrapper for callers
   that have only `options`. The single caller in
   `builder.rs:has_ssh_compression` already has access to
   `self.host`, `self.user`, and can derive `local_user` cheaply.

No wrapper around `ssh2-config` is required because we never adopted
it. If a future sprint reverses that decision, the wrapper layout is
straightforward: `ssh2_config::SshConfig::default().parse(reader)` →
`HostParams::compression: Option<bool>`, with a small adapter to apply
our `Match` decisions on top because `ssh2-config`'s documented
"missing features" list still names `Match`.

## Out of scope (explicit)

- Acting on the parsed value beyond firing the existing warning. SSC-4
  remains advisory.
- `Match exec` and any other condition listed as DEFER.
- `Include` glob expansion is already handled (or not) by SSC-3.impl;
  SSC-4 does not change it.
- Caching parse results across transfers. The probe runs once per
  process via `warn_double_compression_once`; caching adds no value.
- Auto-canonicalization (`CanonicalizeHostname`). Out of scope for the
  warning; ssh will canonicalize at connect time as it always has.

## Follow-up tasks (SSC-4 punch list)

- **SSC-4.b Implementation PR.** Add the `Match` evaluator and
  `MatchContext`, extend `parse_enables_compression`, plumb the context
  through `has_ssh_compression`. Pure-Rust, zero new deps.
- **SSC-4.c Local-user resolution helper.** Tiny cross-platform
  `local_user()` returning `Option<String>` from `USER` /
  `USERNAME`. Place in `config_lookup.rs` next to `home_dir()`.
- **SSC-4.d Fixture tests.** Land the test matrix above as a new
  `crates/rsync_io/tests/ssh_config_match.rs`. Use `EnvGuard` to pin
  `HOME` / `USERPROFILE` / `USER` / `USERNAME` per test.
- **SSC-4.e Docs.** Update the README "Avoid SSH + rsync
  double-compression" section to remove the "`Match` blocks are
  intentionally not evaluated" caveat and replace it with the
  honoured-conditions list. Update `man/oc-rsync.1` likewise.
- **SSC-4.f Memory note refresh.** Mark
  `project_ssh_compression_no_config_parse.md` as updated to reflect
  the `Match`-aware behaviour and the residual `exec` deferral.
