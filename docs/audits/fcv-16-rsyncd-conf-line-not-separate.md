# FCV-16 - rsyncd.conf line / key-value parser is not a separate entry point

Tracking issue: #2449. Companion to `docs/audits/fcv-3-fuzz-coverage-gaps.md`,
`docs/audits/fcv-13-secrets-audit.md`, and `docs/audits/fuzz-coverage-matrix.md`.

## 1. Scope

FCV-16 asked whether the `rsyncd.conf` line / key-value sub-parser warranted a
dedicated fuzz target alongside the whole-file `RsyncdConfig::parse` target
landed in PR #4444 (`fuzz/fuzz_targets/rsyncd_conf.rs`). The hypothesis was that
a finer-grained driver targeting `parse_line` / `parse_key_value` /
`parse_section` / `parse_directive` would shorten the libFuzzer feedback loop
for the per-line state transitions.

## 2. Survey of candidate sub-parsers

`rsyncd.conf` is parsed by two distinct pathways in the daemon crate. Both were
audited for a public line-level entry point.

### 2.1 `daemon::rsyncd_config` (standalone API)

`crates/daemon/src/rsyncd_config/`:

| Item | Visibility | Granularity |
|---|---|---|
| `RsyncdConfig::parse` (`mod.rs:82`) | `pub` | whole file (`&str`) |
| `RsyncdConfig::from_file` (`mod.rs:72`) | `pub` | whole file (`&Path`) |
| `Parser::new` (`parser.rs:20`) | `pub(crate)` | whole file (constructor) |
| `Parser::parse` (`parser.rs:28`) | `pub(crate)` | whole file (loop driver) |
| `Parser::parse_global_directive` (`parser.rs:116`) | private fn | single key/value |
| `Parser::parse_module_directive` (`parser.rs:212`) | private fn | single key/value |
| `Parser::parse_bool` (`parser.rs:356`) | private fn | single value |
| `Parser::parse_list` (`parser.rs:368`) | private fn | single value |

The per-directive, per-bool, and per-list helpers exist but are all private to
`parser.rs`. None are exposed at the module or crate boundary. The only
reachable entry points are the whole-file `RsyncdConfig::parse` and
`RsyncdConfig::from_file` functions.

### 2.2 `daemon::sections::config_parsing` (daemon runtime path)

`crates/daemon/src/daemon/sections/config_parsing/`:

| Item | Visibility | Granularity |
|---|---|---|
| `parse_config_modules` (`parser.rs:8`) | `pub(crate)` | whole file (`&Path`) |
| `parse_config_modules_inner` (`parser.rs:13`) | private fn | whole file + include stack |
| `apply_global_directive` (`global_directives.rs`) | private fn | single key/value |
| `apply_module_directive` (`module_directives.rs`) | private fn | single key/value |
| `apply_include_directive` (`include_merge.rs:9`) | private fn | single key/value |
| `ensure_valid_module_name` (`module_directives.rs`) | private fn | identifier validation |
| `merge_optional_directive` (`include_merge.rs:132`) | private fn | post-include merge |

`parse_config_modules` is the only entry point and is itself `pub(crate)` -
it is not reachable from outside the `daemon` crate. The per-directive helpers
are private to their respective `include!()`-d files. There is also no public
constructor that would let a fuzz target inject a single line into a
`GlobalParseState` directly.

## 3. Decision

**Leave the existing `rsyncd_conf` fuzz target as-is. No code change for
FCV-16.**

Rationale:

1. **No separate sub-parser exists in the public API.** Both pathways
   (`RsyncdConfig::parse` and `parse_config_modules`) drive a single
   line-loop that dispatches to private per-directive helpers. The
   line / key-value handlers are not callable from `fuzz/fuzz_targets/`
   without changing visibility.
2. **The constraint forbids production-code changes.** FCV-16's brief
   explicitly disallows surfacing a new `pub` API solely for fuzzing.
   Adding `pub fn parse_line(...)` to either parser would widen the
   crate's public surface for one consumer (the fuzzer) and create a
   maintenance pin on internal helper signatures.
3. **The whole-file target already exercises every line-level branch.**
   `fuzz/fuzz_targets/rsyncd_conf.rs` feeds arbitrary UTF-8 bytes to
   `RsyncdConfig::parse`. The line loop, `[module]` header parser,
   `key = value` split, `parse_bool`, `parse_list`, and every
   `match key` arm in `parse_global_directive` /
   `parse_module_directive` are all reached by single-line inputs.
   libFuzzer's coverage feedback discovers each `match` arm in seconds
   because inserting the directive keyword strictly grows the coverage
   bitmap.
4. **No measurable speed-up from a finer target.** The whole-file driver
   adds two function calls (`Parser::new`, the `for` loop entry) before
   reaching the same per-line code. The cost is negligible compared to
   libFuzzer's mutation and instrumentation overhead, so a per-line
   driver would not reduce wall-clock time-to-first-crash.
5. **Precedent matches `auth_response` / FCV-13.** That audit reached
   the same conclusion - a single combined target sharing a corpus is
   preferred over surface-expanding splits when the inner parsers are
   not independently reachable.

## 4. Re-evaluation trigger

Re-open FCV-16 only if any of the following hold:

- A refactor exposes a public line-level entry point (e.g.
  `pub fn parse_directive(line: &str) -> Result<Directive, ConfigError>`)
  for reasons independent of fuzzing. At that point a dedicated target
  becomes free and should be added.
- A regression introduces non-trivial per-line state outside the existing
  line loop (e.g. multi-line continuations, here-docs, macro expansion)
  whose branches the whole-file corpus cannot reach within a 10-minute
  fuzzing window.
- A panic or hang is discovered inside one of the private per-directive
  helpers and a minimised reproducer would benefit from a per-line
  driver.

Until then the single binary at `fuzz/fuzz_targets/rsyncd_conf.rs`
satisfies FCV-16's coverage requirement.
