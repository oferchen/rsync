# SSC-5: ssh_config `Host` pattern matching audit

## Summary

`SshCommand::has_ssh_compression()` (`crates/rsync_io/src/ssh/builder.rs:239`)
drives `warn_double_compression_once()` in
`crates/core/src/client/remote/ssh_transfer.rs:296`. SSC-3 wired a
hand-rolled parser at `crates/rsync_io/src/ssh/config_lookup.rs` that
consults `~/.ssh/config` and `/etc/ssh/ssh_config` when the argv check
is inconclusive. SSC-4.b (branch `feat/ssc-4b-match-evaluator`,
unmerged at audit time) is adding a `Match` block evaluator. SSC-5 is
the sibling: confirm whether `Host pattern` blocks - the original
per-host directive - are evaluated.

Verdict: **GAP-DESIGN-PROPOSED**. Per-host blocks are intentionally
short-circuited today; the warning misses any `Compression yes` that
lives inside a `Host` block with a non-`*` pattern, even when the
runtime target host clearly matches the pattern. Three distinct gaps
identified. Recommended fix shape and SSC-5.b subtask are described
below.

## Findings

### 1. Per-host blocks are not evaluated at all

`parse_enables_compression` in `config_lookup.rs:96` walks the file
line-by-line, tracking the active block as one of
`Block::{TopLevel, HostStar, HostOther, Match}`. The only block whose
`Compression` directive is honoured (besides `TopLevel`) is `HostStar`,
where the test is whether the `Host` line contains the literal
single-token `*`:

```rust
"host" => {
    block = if host_patterns_include_star(value) {
        Block::HostStar
    } else {
        Block::HostOther
    };
}
```

`host_patterns_include_star` (line 153) returns true only when one of
the space- or comma-separated tokens equals `"*"` exactly. Every other
pattern (literal hostname, glob, negated token) falls into
`Block::HostOther` and is silently dropped:

```rust
Block::HostStar if host_star.is_none() => host_star = parsed,
_ => {}   // HostOther + Match never set a result
```

A user with the conventional layout

```text
Host web*.prod.example.com
    Compression yes
```

connecting to `web1.prod.example.com` while rsync runs `--compress`
gets no warning, even though SSH will compress the transport in
practice.

### 2. Pattern matcher exists, just not wired here

`crates/rsync_io/src/ssh/embedded/ssh_config.rs` already implements the
full OpenSSH `Host` pattern matcher used by the embedded russh
transport:

- `pattern_matches` (line 147) - glob (`*`, `?`) recursive descent that
  mirrors `fnmatch(3)` without character classes.
- `host_matches_any_pattern` (line 124) - splits the pattern-list on
  whitespace or comma, applies `!`-negation per OpenSSH semantics ("any
  negated match disables the whole line"), and returns true only when
  at least one positive token matches and no negated token matches.
- First-match-wins semantics via `set_if_unset` (line 203) on the
  resolved directives.

Both helpers are `pub(super)`-scoped to the `embedded` module. They are
not re-exported, so `config_lookup` cannot reach them without either a
visibility relaxation or a duplication.

### 3. Call site does not pass the target host

`has_ssh_compression(&self)` (`builder.rs:239`) calls
`config_lookup::ssh_config_enables_compression(&self.options)`. The
parser receives only the SSH option list - it never sees
`self.host`. This is by design today: the doc comment on
`config_lookup.rs:14-19` says per-host blocks are skipped because
"the warning site does not know which host the user is about to connect
to". That premise is wrong: `SshCommand` stores the target host in
`self.host` (`builder.rs:46`) and the call site has `&self` access. The
host has been knowable since SSC-3 landed.

### 4. Pattern type duplication risk (SSC-4.b)

The pending SSC-4.b branch (`feat/ssc-4b-match-evaluator`, commit
`1ae23ceba`) introduces a third pattern abstraction in
`config_lookup.rs`: a `Pattern` struct with `glob` + `negate` fields,
plus a glob matcher `pattern_glob_matches`. That matcher is functionally
identical to `embedded/ssh_config.rs::pattern_matches`; the negation
handling is functionally identical to `host_matches_any_pattern`. The
only meaningful difference is the case-folding policy (SSC-4.b folds
hostnames case-insensitively unconditionally; the embedded matcher is
case-sensitive). After SSC-4.b lands, the workspace has three
overlapping pattern matchers:

| Location | Glob | Negation | Case-folds |
|---|---|---|---|
| `embedded/ssh_config.rs::pattern_matches` + `host_matches_any_pattern` | yes | yes | no |
| `config_lookup.rs::host_patterns_include_star` | no (literal `*` only) | no | no |
| `config_lookup.rs::Pattern` + `pattern_glob_matches` (SSC-4.b) | yes | yes | yes (host) |

SSC-5.b should pick one and delete the others, or extract them into a
single `ssh::pattern` module shared by both `config_lookup` and
`embedded::ssh_config`.

## Cross-check against OpenSSH semantics

`ssh_config(5)` (OpenSSH 9.x) on `Host`:

> Restricts the following declarations (up to the next `Host` or
> `Match` keyword) to be only for those hosts that match one of the
> patterns given after the keyword. If more than one pattern is
> provided, they should be separated by whitespace. A single `*` as a
> pattern can be used to provide global defaults for all hosts. The
> host is usually the *hostname* argument given on the command line
> (see the `CanonicalizeHostname` keyword for exceptions).

Plus the `PATTERNS` section:

> A pattern consists of zero or more non-whitespace characters, `*` (a
> wildcard that matches zero or more characters), or `?` (a wildcard
> that matches exactly one character). [...] A pattern entry may be
> negated by prefixing it with an exclamation mark (`!`). If a negated
> entry is matched, then the Host entry is ignored, regardless of
> whether any other patterns on the line match.

The current `config_lookup` matcher honours only the "global defaults"
sentence and ignores the rest. The embedded matcher (`embedded/ssh_config.rs`)
implements the full grammar above.

## Gap inventory

| # | Gap | Severity | Fix size |
|---|---|---|---|
| G1 | Per-host blocks (`Host foo`, `Host web*`, `Host !x *`) are not evaluated | Functional: missed warnings | Non-trivial: requires threading the target host through `has_ssh_compression -> ssh_config_enables_compression -> parse_enables_compression`, then per-block pattern evaluation |
| G2 | `host_matches_any_pattern` + `pattern_matches` exist in `embedded/ssh_config.rs` but are not re-used by `config_lookup.rs` | Duplication risk | Trivial once visibility opened; or extract to shared `ssh::pattern` module |
| G3 | After SSC-4.b lands, three overlapping pattern matchers coexist | Maintenance debt | Trivial cleanup; pick one and delete the rest |

## Proposed fix shape (SSC-5.b)

Sketch only; implementation is the SSC-5.b deliverable.

1. **Add a shared `ssh::pattern` module** with one `Pattern` type and one
   `host_matches_any_pattern(host, patterns)` free function. Source the
   implementation from `embedded/ssh_config.rs` (it is already
   production-tested). Re-export from `super::pattern` so both
   `config_lookup` and `embedded::ssh_config` can consume it.
   Case-folding policy: opt-in per call to support SSC-4.b's
   hostname-case-insensitive Match evaluation while preserving the
   embedded `Host` matcher's existing behaviour.

2. **Extend `Block` in `config_lookup.rs`** to carry the active
   `Host`-line pattern set, not just a discriminant:

   ```rust
   enum Block {
       TopLevel,
       Host(Vec<Pattern>),
       Match(Vec<MatchCondition>),   // SSC-4.b
   }
   ```

   The current `HostStar` / `HostOther` split disappears - a `Host *`
   block becomes `Host(vec![Pattern::new("*")])`.

3. **Thread the target host through the public API**:

   ```rust
   pub(super) fn ssh_config_enables_compression(
       options: &[OsString],
       host: &str,                  // new
   ) -> bool;

   pub(super) fn parse_enables_compression(
       text: &str,
       host: &str,                  // new
   ) -> bool;
   ```

   Update the call in `builder.rs:250`:

   ```rust
   super::config_lookup::ssh_config_enables_compression(
       &self.options,
       &self.host.to_string_lossy(),
   )
   ```

   `self.host` is already `OsString`; `to_string_lossy()` is enough
   for ASCII hostnames which is what OpenSSH supports anyway.

4. **First-match-wins per directive, per OpenSSH semantics**:
   compression resolution becomes "scan top-to-bottom; remember the
   first `Compression yes|no` whose enclosing block matches; return
   that". This generalises the current per-`Block`-bucket logic and
   collapses naturally with SSC-4.b's `Match` evaluator since both
   share the same gating predicate.

5. **Test fixtures** - add to the existing `tests/ssh_config_compression.rs`
   integration suite:
   - `Host web*.example.com` + connect to `web1.example.com` -> true
   - `Host web*.example.com` + connect to `db.example.com` -> false
   - `Host !banned.example.com *` + connect to `banned.example.com` -> false
   - `Host !banned.example.com *` + connect to `ok.example.com` -> true
   - `Host alpha` then `Host *` first-match-wins: `alpha` block's
     `Compression no` wins over `Host *` `Compression yes`.

Estimated LoC: ~120 added + ~40 removed across the three files, plus
~80 test LoC. Above the 30-LoC threshold for a "trivial gap" fix; this
audit closes SSC-5 and SSC-5.b carries the implementation.

## Coordination with SSC-4.b/.c

SSC-4.b is unmerged and adds the `Pattern` + `MatchContext` machinery
in `config_lookup.rs` SSC-5 needs. SSC-5.b should rebase on top of
SSC-4.c (which wires `MatchContext` into `parse_enables_compression`)
so both Match and Host blocks consume the same `MatchContext`-equivalent
target host string. The recommended sequencing is:

1. SSC-4.b lands (Pattern + evaluate_match additions).
2. SSC-4.c lands (MatchContext plumbed into parse_enables_compression).
3. SSC-5.b lands (Host block evaluator), reusing SSC-4.b's `Pattern`
   type and SSC-4.c's `MatchContext::host` field as the single source
   of "what host are we connecting to".
4. SSC-5.c (optional follow-up) extracts the shared `ssh::pattern`
   module and removes `embedded/ssh_config.rs::pattern_matches`
   duplication.

## References

- `crates/rsync_io/src/ssh/builder.rs:239` - `has_ssh_compression()`
- `crates/rsync_io/src/ssh/config_lookup.rs:96` - `parse_enables_compression()`
- `crates/rsync_io/src/ssh/config_lookup.rs:153` - `host_patterns_include_star()`
- `crates/rsync_io/src/ssh/embedded/ssh_config.rs:124` - `host_matches_any_pattern()`
- `crates/rsync_io/src/ssh/embedded/ssh_config.rs:147` - `pattern_matches()`
- `crates/core/src/client/remote/ssh_transfer.rs:296` - `warn_double_compression_once()` call site
- `docs/design/ssc-4a-match-conditions.md` - SSC-4 design context
- Branch `feat/ssc-4b-match-evaluator`, commit `1ae23ceba` - in-flight SSC-4.b
- `ssh_config(5)`, OpenSSH 9.x - `Host` and `PATTERNS` sections
