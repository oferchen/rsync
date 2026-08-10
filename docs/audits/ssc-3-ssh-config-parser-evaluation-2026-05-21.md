# SSC-3 - ssh_config parser evaluation for double-compression detection

Date: 2026-05-21
Scope: documentation-only audit; no code, no `Cargo.toml`, no dependency changes.
Tracks: SSC follow-up to PR #4667 (SSC-1 argv warning) and PR #4655 (SSC-2 README documentation).
Reads: `crates/rsync_io/src/ssh/builder.rs` (current detection surface).

## 1. Problem restatement

Today `SshCommand::has_ssh_compression()` (`crates/rsync_io/src/ssh/builder.rs:236`)
inspects the explicit argv that rsync hands to the SSH client. It recognises:

- `-C`
- `-oCompression=<truthy>` and `-o Compression=<truthy>` (split across two argv
  positions), case-insensitive, truthy = `yes` / `true` / `1`.

The caller at `crates/core/src/client/remote/ssh_transfer.rs:296` combines that
predicate with rsync's own `--compress` and, when both are active, emits a
one-shot stderr warning (SSC-1, PR #4667). SSC-2 (PR #4655) documents the same
hazard in the README.

The doc comment at `builder.rs:228-230` is explicit about the limitation:

> Detection is conservative and inspects the explicit command-line arguments
> only - it does not parse `~/.ssh/config`, since the SSH client merges that
> file at spawn time and we cannot reliably read it.

A user with `Compression yes` in `~/.ssh/config` or `/etc/ssh/ssh_config`,
launching `oc-rsync --compress user@host:/src /dst` without `-e ssh -C`, gets
no warning. The SSH transport silently double-compresses (rsync's deflate
stream is then run through SSH's zlib), wasting 20-40% CPU on already-
compressed bytes per the project memory note `project_ssh_compression_no_config_parse.md`.

SSC-3 asks: should we close the argv-only gap by parsing OpenSSH client config
ourselves?

## 2. Candidate crates

Metadata collected from lib.rs on 2026-05-21. Maintenance heuristic:
"Active" = release within the last 12 months; "Stale" = last release > 24
months ago.

### 2.1 `ssh2-config` (veeso)

- **Version / release:** 0.7.1, 2026-04-26.
- **Downloads:** ~6.5K / month.
- **License:** MIT.
- **Repository:** https://github.com/veeso/ssh2-config.
- **Runtime deps (6):** `bitflags 2`, `dirs 6`, `glob 0.3`, `log 0.4`,
  `thiserror 2`, `wildmatch 2`.
- **Footprint:** ~4K SLoC in-crate; dep closure pulls `dirs` (which brings
  `dirs-sys` -> `libredox` / `option-ext` / `windows-sys` on Windows). Net add
  to the workspace dep graph is in the 10-20 crate range counting transitives.
- **Maintenance:** Active. Rust 2024 edition. Has `nolog` feature.
- **Compression keyword:** supported (listed among 23 attributes).
- **Match / Include:** **not supported.** Upstream README documents
  "Match patterns" as a known limitation. `Include` is not mentioned and was
  not observable in the public API surface.
- **Host patterns:** supported (glob via `wildmatch`); first-match-wins,
  matching OpenSSH semantics for top-down precedence.
- **System file precedence:** caller must parse `~/.ssh/config` and
  `/etc/ssh/ssh_config` separately and merge results. Crate does not handle
  the precedence chain end-to-end.

### 2.2 `ssh2-config-rs` (prizz, fork of 2.1)

- **Version / release:** 0.7.2, 2026-01-26.
- **Downloads:** ~1.2K / month.
- **License:** MIT.
- **Repository:** https://github.com/prizz/ssh2-config-rs.
- **Runtime deps (5):** same as `ssh2-config` minus the `ssh2` dev dep.
- **Footprint:** 4K SLoC, ~180KB.
- **Maintenance:** Active fork; the explicit rebrand goal is "no OpenSSL,
  works with russh."
- **Compression keyword:** supported.
- **Match / Include:** Match unsupported; Include not documented.
- **Differs from 2.1:** mostly the dependency closure (no `ssh2` / OpenSSL
  link).

### 2.3 `ssh_config` (indek)

- **Version / release:** 0.1.0, 2020-01-10.
- **Downloads:** ~155 / month.
- **License:** MPL-2.0.
- **Repository:** https://gitlab.com/indek/ssh_config.
- **Runtime deps:** 0.
- **Footprint:** ~567 SLoC, 26KB.
- **Maintenance:** **Stale.** Last release > 6 years ago.
- **Compression / Match / Include:** not documented; the README says only
  "parses OpenBSD ssh_config files." Would require a code audit.
- **License caveat:** MPL-2.0 is acceptable for a Cargo dep but is the only
  copyleft option in the list and the only one not already represented in the
  workspace.

### 2.4 `russh-config` (Eugeny / Russh ecosystem)

- **Version / release:** 0.58.0, 2026-03-18.
- **Downloads:** ~6.4K / month.
- **License:** Apache-2.0.
- **Repository:** https://github.com/Eugeny/russh (sub-crate).
- **Runtime deps (6):** `futures 0.3`, `globset 0.4`, `log 0.4`,
  `thiserror 2`, `tokio 1` (with `io-util,net,macros,process`), `whoami 1`.
- **Footprint:** small parser but the `tokio` dep is the killer - we'd pull
  the async runtime into `rsync_io` just to parse a text file.
- **Maintenance:** Active.
- **Compression keyword:** not documented as supported; surface is geared at
  ProxyCommand resolution for russh, not arbitrary keyword lookup.
- **Match / Include:** not documented.
- **Verdict:** wrong fit. Designed to feed Russh's connect path, not a
  general keyword query API. Hard pass on dep grounds alone.

### 2.5 Other candidates considered and rejected

- **`sks-ssh2-config`:** writer-focused (preserves ordering for round-trip);
  has only the same parse coverage as `ssh2-config`. Not justified.
- **`openssh` crate:** scripts ssh(1); does not expose config parsing.
- **`ssh` (russh re-export):** SSH client, not a parser.

## 3. Roll-our-own option

A bespoke parser scoped to "look up `Compression` for hostname H" would not
need anything close to the full OpenSSH grammar. Estimated scope:

| Feature | LoC (est.) | Required for SSC-3? |
|---------|------------|--------------------|
| Tokeniser (whitespace + quoting + `\\` continuation) | ~40 | yes |
| `Host <patterns>` blocks with `*`, `?`, `!` negation | ~60 | yes |
| Top-down first-match precedence per key | ~25 | yes |
| `Compression yes/no` boolean parse (case-insensitive) | ~15 | yes |
| `~/.ssh/config` then `/etc/ssh/ssh_config` merge | ~25 | yes |
| `Include` (glob, recursion depth cap, cycle guard) | ~70 | nice-to-have |
| `Match host/exec/user/...` | ~150+ | no - exec is a security/perf footgun |
| Token expansion (`%h`, `%p`, `%u`...) | ~50 | no (not needed for Compression) |
| Tests (positive, negative, precedence, malformed input) | ~150 | yes |

**Minimum viable parser:** ~200 LoC + ~150 LoC tests, no `Include`, no
`Match`. Lives forever in `crates/rsync_io/src/ssh/config.rs`. Owned bug
surface, owned platform quirks (Windows file paths, OpenSSH-on-Windows
location), owned upstream-drift exposure.

**With `Include` (recommended floor if we ship this at all):** ~270 LoC
+ ~200 LoC tests.

**With `Match`:** scope explodes. `Match exec` shells out to an arbitrary
command, which we would have to either honour (security review, sandboxing,
timeout semantics) or quietly ignore (silent under-detection - the exact
failure mode SSC-3 is meant to fix).

## 4. Decision matrix

| Axis | Skip (status quo: argv + README) | Adopt `ssh2-config(-rs)` | Roll our own (MVP) |
|------|----------------------------------|--------------------------|--------------------|
| Detection coverage | argv only | argv + `~/.ssh/config` + `/etc` (no Match) | argv + `~/.ssh/config` + `/etc` (no Match) |
| New deps | 0 | +5-6 direct, ~15 transitive | 0 |
| Code we maintain | 0 | 0 (but pin / audit upstream) | ~200-400 LoC permanent |
| Unsafe added | 0 | 0 | 0 |
| MSRV risk | 0 | low - both crates target recent stable | 0 |
| License | n/a | MIT (both) | n/a |
| Match / exec parity | none | none (gap remains) | none (gap remains) |
| Upstream rsync parity | exact - upstream does not parse either | **deviation** - upstream parses neither | **deviation** - upstream parses neither |
| Failure mode if parse wrong | n/a | false-positive warning -> user noise | false-positive warning -> user noise |
| User-visible impact | rare miss (see Section 5) | catches the missed case but adds noise risk | same as ssh2-config |

The "upstream parity" row is decisive in two directions. First, upstream rsync
3.4.x deliberately does not parse `~/.ssh/config`, so deviating means we own a
behaviour upstream users will not have a mental model for. Second, since
SSC-1's job is an *informational warning*, any false positive ("you have
Compression yes for some other host pattern but not this one") becomes
operator noise that erodes trust in the warning itself.

## 5. Frequency of the missed case

From `project_ssh_compression_no_config_parse.md` and the OpenSSH default
review:

- OpenSSH `Compression` defaults to `no` and has for many releases (the
  KEX_DEFAULT_COMP myproposal lists `none` first).
- Users who flip `Compression yes` in `~/.ssh/config` are typically chasing
  slow links and tend to know they did it.
- The remaining at-risk population is users who (a) inherited a config from a
  dotfile repo, (b) added `--compress` to oc-rsync later, and (c) never set
  `-e ssh -C` explicitly. Plausible but not common.
- For everyone in case (c), the SSC-2 README note already tells them where to
  look; the SSC-1 argv warning catches the case they explicitly opted into.

Severity when missed: 20-40% CPU waste on the SSH process, no correctness
impact, no data loss, no interop breakage.

## 6. Recommendation: **SKIP**

Do not add an `ssh_config` parser - neither a third-party crate nor an
in-tree implementation - at this time. The combination of:

- upstream rsync not parsing the file either (parity is a real feature for an
  interop-driven project),
- the low frequency of the missed case (Compression default off; users who
  flip it usually know),
- SSC-1 (argv) + SSC-2 (README) already covering the explicitly-configured
  and the educated-user paths,
- the false-positive risk introducing operator noise that would degrade the
  trust earned by the SSC-1 warning, and
- a non-trivial dependency or maintenance bill in every alternative,

argues for closing SSC-3 as "wontfix - covered by SSC-1+SSC-2."

## 7. Conditions that would reverse this decision

If any of the following becomes true, re-open and prefer
`ssh2-config-rs` (smallest dep delta of the viable options, MIT, no OpenSSL
link, active maintenance, parity with `ssh2-config` minus the `ssh2`
build dep):

1. Bug reports or community evidence show users routinely hit silent
   double-compression in the wild (>= a handful of issues, not single anecdote).
2. We adopt russh (`crates/rsync_io/src/ssh/embedded/`) by default and the
   transport itself starts reading `~/.ssh/config` for connect-time params -
   at that point a parser already exists in the dep graph and the marginal
   cost of querying `Compression` is near zero.
3. Upstream rsync starts parsing `~/.ssh/config` (memory says: not as of
   3.4.2 / 3.4.3 CVE batch; recheck on each upstream import).

Until then: keep `has_ssh_compression()` argv-only, keep the SSC-2 README
note, and update the doc comment at `builder.rs:228-230` to point at this
audit so future readers find the rationale instead of re-asking the question.

## 8. Cross-references

- Source: `crates/rsync_io/src/ssh/builder.rs:222-241`
  (`has_ssh_compression`).
- Caller: `crates/core/src/client/remote/ssh_transfer.rs:296`.
- SSC-1 PR: #4667 (argv warning, in CI as of 2026-05-21).
- SSC-2 PR: #4655 (README documentation, merged).
- Memory: `project_ssh_compression_no_config_parse.md`.

## 9. Sources

- ssh2-config: https://lib.rs/crates/ssh2-config (v0.7.1, 2026-04-26).
- ssh2-config-rs: https://lib.rs/crates/ssh2-config-rs (v0.7.2, 2026-01-26).
- ssh_config: https://lib.rs/crates/ssh_config (v0.1.0, 2020-01-10).
- russh-config: https://lib.rs/crates/russh-config (v0.58.0, 2026-03-18).
- OpenSSH 6.6 release notes: https://www.openssh.org/txt/release-6.6.
- OpenSSH 7.6 release notes: https://www.openssh.org/txt/release-7.6.
- "Can we disable SSH compression by default?" openssh-unix-dev, 2019-02:
  https://lists.mindrot.org/pipermail/openssh-unix-dev/2019-February/037587.html.
- `ssh_config(5)` man page: https://linux.die.net/man/5/ssh_config.
