# Differential fuzzer for filter rules vs upstream rsync

Detail design for a differential fuzzing harness that compares oc-rsync's
filter-rule evaluation byte-for-byte against upstream rsync 3.4.1 over a
randomized cross-product of (filter-rule-set, on-disk tree, transfer mode).
This note specifies the harness shape, the input grammar, the equivalence
oracle, the seed corpus, the daemon-server differential mode, the resource
budget, and the wire-compat invariants that the harness MUST NOT violate.

This is a design note. No Rust code lands in this PR. Implementation is
tracked in the follow-up TODOs enumerated at the end of this document.

## 1. Problem statement

Filter rules are the most subtly-behaved subsystem in rsync. The grammar
parsed by `crates/filters/src/merge/parse.rs` and the evaluator built
from `crates/filters/src/compiled/rule.rs` honour twelve interacting
features in roughly this order:

1. Action prefix (`+`, `-`, `P`, `R`, `H`, `S`, `!`, long-form keywords
   `include`/`exclude`/`protect`/`risk`/`hide`/`show`/`clear`).
2. Modifier flags between action and pattern (`!`, `p`, `s`, `r`, `x`,
   `e`, `n`, `w`, `C`, `P` for perishable in long form).
3. Anchoring via leading `/`, evaluated relative to the transfer root.
4. Directory-only matching via trailing `/`.
5. Recursive wildcards (`**`) versus single-segment wildcards (`*`).
6. Implicit `**/` prefix for unanchored patterns without internal `/`.
7. Negation through the `!` modifier (rule body inverted, not action).
8. Per-side applicability (`s` sender-only, `r` receiver-only) carved
   out by `applies_to_sender` / `applies_to_receiver` in
   `crates/filters/src/rule.rs`.
9. Word-split (`w`) splitting one rule line into many.
10. Per-directory merge (`:`) and one-shot merge (`.`) directives parsed
    by `crates/filters/src/merge/parse.rs`.
11. Daemon-side filter injection layered before client filters by
    `crates/daemon/src/daemon/sections/module_access/helpers.rs:223`
    (`build_daemon_filter_rules`).
12. Clear (`!`) directives that remove side-applicability flags from
    earlier rules per `crates/filters/src/compiled/clear.rs`.

Static golden tests in `crates/filters/tests/` (27 files at the time of
writing, including `proptest_rule_evaluation.rs`, `proptest_fuzz.rs`,
`filter_chain_edge_cases.rs`, `complex_filter_scenarios.rs`) cover known
combinations. Property tests cover algebraic shapes the authors thought
to encode. Differential fuzzing finds the cases nobody thought to write.

The goal: catch unknown divergences from upstream before users do, by
generating random valid filter input plus a random tree, executing both
implementations under identical flags, and asserting byte-identical
inclusion decisions over the entire tree.

## 2. Scope

### 2.1 In scope

- **Pattern syntax**: full grammar accepted by `parse_rule_line_expanded`
  in `crates/filters/src/merge/parse.rs`, including all action prefixes,
  all modifier flags, anchored and unanchored patterns, dir-only suffix,
  glob metacharacters (`*`, `**`, `?`, `[abc]`, `[!abc]`, character
  ranges `[a-z]`).
- **Filter sources**: CLI `--filter`, `--exclude`, `--include`,
  `--exclude-from`, `--include-from`, per-directory `.rsync-filter`
  merge files via `crates/filters/src/chain.rs::DirFilterGuard`,
  one-shot merges via `merge`/`.` directives.
- **Daemon directives**: `rsyncd.conf` `filter`, `exclude`, `include`,
  `exclude from`, `include from` parameters consumed by
  `crates/daemon/src/rsyncd_config/sections.rs:155-159` and assembled
  into the daemon filter list at
  `crates/daemon/src/daemon/sections/module_access/helpers.rs:223-280`.
  This addresses the gap noted in #1366 and the daemon hide/show
  modifier semantics described in #1696.
- **Modes**:
  - **Local mode**: `oc-rsync --dry-run` versus `rsync --dry-run`,
    both reading the same source tree with identical CLI filters.
  - **Daemon mode**: `oc-rsyncd` versus upstream `rsync --daemon`,
    each with the same `rsyncd.conf` filter directives, with the
    client requesting transfer; verify both daemons impose the same
    server-side filtering before the file list crosses the wire.
  - **Client/server cross-product**: client-imposed filters layered on
    top of daemon-imposed filters; the two are evaluated independently
    by upstream (see `clientserver.c:rsync_module()` cited at
    `helpers.rs:213-222`) and oc-rsync must match.

### 2.2 Out of scope

- Bit-perfect log output. Verbose logging diverges between
  implementations on cosmetic details (path quoting, escape forms);
  the oracle compares the *set* of transferred files, not log strings.
- Performance. The harness is a correctness oracle. Throughput is
  irrelevant; CPU is the limiting factor, not network.
- xattr filtering (`x` modifier). xattr-only rules short-circuit before
  compilation per `crates/filters/src/compiled/mod.rs:48` and require
  xattr-aware fixtures. Tracked separately.

## 3. Mutation strategies

The harness uses the `arbitrary` crate (already a dependency of
`crates/filters/fuzz/Cargo.toml`) to derive structured input from the
fuzzer's byte stream. Three independent mutators feed a single
`FuzzInput` struct.

### 3.1 Pattern mutation

A grammar-based generator emits valid filter rule lines. The grammar
mirrors the dispatch in
`crates/filters/src/merge/parse.rs::parse_rule_line_expanded`:

```
rule_line  := action_prefix modifiers? SP pattern
            | long_keyword SP modifiers? SP pattern
            | clear_directive
            | merge_directive
action_prefix := "+" | "-" | "P" | "R" | "H" | "S"
long_keyword  := "include" | "exclude" | "protect" | "risk"
                | "hide" | "show" | "clear" | "merge" | "dir-merge"
modifiers     := ("!"|"p"|"s"|"r"|"e"|"n"|"w"|"C"|"P")+
pattern       := anchor? glob_segment ("/" glob_segment)* dir_suffix?
anchor        := "/"
dir_suffix    := "/"
glob_segment  := ("*" | "**" | "?" | char | char_class)+
char          := letter | digit | "_" | "-" | "."
char_class    := "[" "!"? char_range+ "]"
char_range    := char | char "-" char
clear_directive := "!"
merge_directive := ("." | ":") modifiers? SP path
```

Length bound: 1 to 50 rules per generated set, distribution biased
toward shorter sets so coverage of small interactions dominates. Each
rule line is bounded at 256 bytes; pathological lines are still parsed
but the size cap protects the upstream `rsync` child process.

### 3.2 Tree mutation

A bounded random directory tree is generated alongside each rule set:

- **Width**: 0-8 entries per directory (Poisson, mean 3).
- **Depth**: 0-6 levels.
- **File-count cap**: 10,000 entries total. The generator aborts the
  current iteration if it would exceed the cap, so tail latency is
  bounded.
- **Filename alphabet**: weighted union of `[a-z0-9_-]`, `.`, embedded
  spaces, embedded tabs, single trailing dot, leading dot ("hidden"),
  selected NFC-canonical UTF-8 codepoints (Latin-1 supplement, CJK,
  combining marks). Filenames containing `/` or NUL are rejected at
  generation time; backslash is rejected on Windows fixtures.
- **Entry kinds**: regular files (90%), directories (8%), symlinks
  (2% on POSIX; skipped on Windows where the test runner lacks the
  required privilege). Block devices and FIFOs are out of scope: they
  do not interact with filter rules and their creation requires root.
- **Content**: zero-length. Filter rules never observe content; padding
  files would only slow the harness.

A separate `walkdir`-based normaliser writes both trees into temp
directories with byte-identical inode order (sorted) so any
walker-order ambiguity in the tested binaries surfaces as a reproducible
divergence rather than a flake.

### 3.3 Merge file mutation

Per-directory `.rsync-filter` files are themselves mutated content:

- File presence: 0-30% of directories receive a merge file.
- Body: between 0 and 20 rule lines, drawn from the same grammar as
  3.1, plus comment lines (`#` or `;` prefix) at random positions and
  blank lines at random positions to exercise the line-skip logic at
  `crates/filters/src/merge/parse.rs:45`.
- Inheritance: random `n` (no-inherit) modifier on the parent
  `dir-merge` directive, exercising the inheritance toggle at
  `chain.rs::DirFilterGuard`.
- Continuation lines: rsync does not honour backslash continuation in
  filter files, but generated input includes lines ending in `\` to
  confirm both implementations treat them as literal trailing
  backslashes (regression for #1062-class path-escaping).

## 4. Equivalence oracle

For each generated `(filter_set, tree, mode)` triple the harness:

1. Materialises the tree under two sibling tempdirs `src/` and `dst/`
   (only `src/` is populated; `dst/` is created empty).
2. Constructs a CLI invocation:
   `<binary> --dry-run --itemize-changes --recursive --no-times \
   <filter-flags> src/ dst/`
   The `--no-times` flag avoids the quick-check pitfall noted in
   project memory (`MEMORY.md` -> Test Flakiness): identical mtimes
   would otherwise mask transfer decisions.
3. Runs both binaries with the same `LANG=C.UTF-8`, the same `umask`,
   and the same `RSYNC_PROTOCOL=32` so protocol negotiation cannot
   fork behaviour.
4. Parses the itemize output. The decision oracle is
   *the set of paths that would be transferred*, derived from lines
   beginning with the upstream itemize prefix (`>`, `<`, `c`, `h`, `*`)
   or, when `--dry-run` short-circuits before itemize, the
   `--list-only` output. Only the path column is compared; flag
   columns vary on cosmetic axes.
5. Asserts:
   - `transfer_set(oc-rsync) == transfer_set(rsync)` as ordered sets
     after sort (path lexicographic order).
   - `delete_set(oc-rsync) == delete_set(rsync)` when `--delete` is in
     the flag mix (every fourth iteration).
   - `protect_set(oc-rsync) == protect_set(rsync)` when `--protect`
     rules are present.
6. On mismatch, dumps:
   - The full filter rule set (one rule per line, exact bytes).
   - The full tree (`find src/ -printf '%y %p\n' | sort`).
   - The symmetric-difference of the two transfer sets.
   - Both binaries' stdout/stderr, gzip-compressed.
   - The cargo-fuzz tmin-shrunk minimal reproducer.

The oracle target lives in
`crates/filters/src/compiled/rule.rs::CompiledRule::matches` and
`crates/filters/src/decision.rs::decision`. Both functions are
read-only and side-effect-free; the harness can call them directly in
addition to the binary-level differential, providing an inner ring of
unit-level fuzzing for free.

## 5. Special cases to seed

The fuzzer's seed corpus is NOT random; it encodes known-tricky inputs
distilled from the existing `crates/filters/tests/` suite, the merge
fuzz corpus at `crates/filters/fuzz/corpus/filter_parse/`, and the
historical bug record. Each seed is a single deterministic
(rule-set, tree) pair.

### 5.1 Trailing-slash semantics

`man rsync` (FILTER RULES section): "if the pattern ends with a /
then it will only match a directory". Seeds:

- `foo` versus `foo/` against a tree containing `foo` (file) and
  `foo/` (directory). Upstream excludes only the directory in the
  second form; the file must remain.
- `foo/` versus `/foo/` at the transfer root and at depth 2.
- `**/` (excludes every directory) versus `**` (excludes every entry).

### 5.2 `**` versus `*` recursion

- `*.log` matches at every depth (implicit `**/` prefix per
  `compiled/mod.rs:55`).
- `/*.log` matches only top-level.
- `**.log` is NOT special; the `**` only spans `/` when bracketed by
  `/` characters per upstream globbing. Seeds verify that
  `a**b` matches `aXXb` but not `a/X/b`.

### 5.3 Re-include after broad exclude

- `+ /important/`, `- *` together with a tree where `important/`
  contains nested directories. Upstream traversal halts at the
  exclude unless the directory ancestors are explicitly included.
  Seeds cover the canonical
  `+ */`, `+ *.txt`, `- *` recipe documented in `man rsync`.

### 5.4 Side-applicability modifiers

- `s` (sender-only). Seeded against trees with files differing only on
  one side. The modifier's interaction with daemon-server filters is
  the failure mode flagged in #1696: when the daemon serves as sender,
  client-side `r`-modifier rules must be ignored by the daemon, and
  vice versa.
- `r` (receiver-only). Same construction with sides flipped.
- `e` (exclude-only). Tests that an `include`-action rule with `e`
  modifier is treated as exclude per
  `crates/filters/src/rule.rs::FilterRule::exclude_only`.

### 5.5 Merge file edge cases

- Empty `.rsync-filter` (zero bytes).
- Merge file containing only comments.
- Merge file containing only blank lines.
- Merge file referencing another merge file (nested `.` directive).
- Merge file with the `n` modifier on the parent `dir-merge`,
  blocking inheritance.
- Merge file with `e` modifier on `dir-merge`, forcing all rules in
  it to be excludes regardless of sign.

### 5.6 Path-escaping CVE-class (cite #1062)

The path-escaping audit in #1062 enumerated a set of path forms that
must NOT escape the transfer root regardless of filter outcome:

- `..`, `../foo`, `foo/../../bar`, `./foo`, `foo/./bar`.
- Embedded NUL byte (rejected at filename generation; included as a
  parser-only seed for the merge file body).
- Backslash as path separator (Windows-style, must be treated as a
  literal character on POSIX).
- Mixed-encoding filenames (UTF-8 source, Latin-1 pattern) -- the
  oracle MUST canonicalise to UTF-8 NFC before comparison; see the
  encoding mitigation in section 12.

## 6. Implementation site

Two options were considered:

- **Extend** `crates/filters/fuzz/`. Existing target `fuzz_filter_chain`
  already builds a `FilterSet` from arbitrary input and evaluates
  paths against it. Adding a binary-differential target alongside it
  is a one-file change.
- **New crate** `crates/fuzz-harness/filters/` parallel to the
  protocol fuzz harness designed in #1193.

Recommendation: **extend the existing fuzz crate**. The protocol fuzz
harness (#1193) targets wire frames, which require a controlled
network channel and a paired protocol replay. The filter differential
needs neither; it needs only the `filters` crate plus a child-process
spawner. Co-locating with the existing parse/chain targets keeps the
seed corpus contiguous and lets all targets share the same
`arbitrary::Arbitrary` derives.

The new fuzz target lives at
`crates/filters/fuzz/fuzz_targets/fuzz_filter_differential.rs`. It
shells out to `rsync` and `oc-rsync` via `std::process::Command`;
binary discovery uses the `RSYNC_BIN` and `OC_RSYNC_BIN` env vars
exported by the same `tools/ci/run_interop.sh` harness that already
locates the upstream binaries used by integration tests.

## 7. Wire-compat invariants

- Filter evaluation is read-only with respect to wire bytes. The
  harness invokes `--dry-run` exclusively; no signature, no checksum,
  no token bytes are emitted on a real socket.
- The daemon-server differential (section 8) uses an in-process
  `loopback:0` listener and `--bwlimit=0`; the only bytes exchanged
  are the file list, which is the artefact the oracle compares.
- No CompiledRule field, no FilterSet API, no merge-file parser entry
  point is mutated. The harness only consumes the public surface
  re-exported from `crates/filters/src/lib.rs:118-127`.
- Production code under `crates/filters/src/`, `crates/daemon/src/`,
  `crates/transfer/src/generator/filters.rs`, and
  `crates/core/src/client/remote/flags.rs` remains untouched.

## 8. Daemon-server differential

A second harness mode exercises the daemon-server filter cross-product.
The setup mirrors the existing daemon interop scaffolding at
`scripts/rsync-interop-server.sh` and `tools/ci/run_interop.sh`:

1. Generate a `rsyncd.conf` containing one module with a random
   selection of `filter`, `exclude`, `include`, `exclude from`,
   `include from` directives (grammar from section 3.1).
2. Generate the `--filter`/`--exclude`/`--include` CLI argument set
   for the client invocation.
3. Spawn upstream `rsync --daemon --no-detach --port 0` with the
   generated config. Capture the assigned port via the `pid file`
   side-channel.
4. Spawn `oc-rsyncd --no-detach --port 0` with the same config.
5. From the client, run
   `rsync --dry-run --itemize-changes rsync://127.0.0.1:<port>/<mod>/ dst/`
   against each daemon. Diff the file list.
6. Optionally capture wire bytes via tcpdump on the loopback
   interface (cite #2075 -- the daemon-side wire-evidence harness).
   The capture is not part of the oracle but is dumped on mismatch
   as forensic data.

Assertion: the file list reaching the client is identical between
oc-rsyncd and upstream rsyncd, confirming that
`build_daemon_filter_rules` at `helpers.rs:223` produces a chain
semantically equal to upstream's `daemon_filter_list`
(`clientserver.c:874-893`).

## 9. Resource budget

| Knob | Per-PR CI | Nightly |
|------|-----------|---------|
| Iterations | 1,000 | 100,000 |
| Wall-clock cap | 90 s | 30 min |
| Tree size cap | 10,000 entries | 10,000 entries |
| Rule set cap | 50 rules | 50 rules |
| Merge file cap per tree | 50 files | 50 files |
| Daemon mode iters | 100 | 5,000 |
| Memory cap (RSS) | 512 MiB | 1 GiB |

The harness is CPU-bound: each iteration spawns two short-lived child
processes (`oc-rsync --dry-run` and `rsync --dry-run`) and compares
text output. No network. No persistent disk state -- everything in
`tempfile::TempDir` cleared per iteration. Daemon-mode iterations are
1/10 the count because daemon spawn dominates wall time.

CPU parallelism: cargo-fuzz drives one libfuzzer process per core via
`-fork=N`. The harness sets `FUZZ_JOBS=$(nproc)` in the nightly
workflow.

## 10. Failure handling

On divergence the libfuzzer worker writes a crash artefact to
`crates/filters/fuzz/artifacts/fuzz_filter_differential/crash-<hash>`
and exits non-zero. The CI workflow:

1. Runs `cargo fuzz tmin -O fuzz_filter_differential <crash-file>`
   to shrink the input to the smallest reproducer.
2. Decodes the shrunk input back to (rule-set, tree) text via the
   inverse of the `Arbitrary` impl.
3. Writes the reproducer into
   `crates/filters/tests/regression_diff_<short-hash>.rs` as a new
   nextest case that drives the same comparison without the fuzzer
   wrapper. This converts every found bug into a permanent
   regression test, mirroring the policy from the protocol fuzz
   harness (#1193).
4. Uploads the crash artefact, the shrunk reproducer, and the
   regression-test stub as workflow artefacts so a maintainer can
   land them in a follow-up PR.

The workflow does NOT auto-commit regression tests; that is a human
review step. The artefact upload is sufficient.

## 11. Activation in CI

A new opt-in workflow `.github/workflows/fuzz-filter-differential.yml`
runs on:

- `workflow_dispatch` (manual trigger).
- `schedule: cron '17 3 * * *'` (nightly at 03:17 UTC, offset from
  the existing benchmark workflow).
- `pull_request` with the `fuzz-required` label, mirroring the model
  established for the protocol fuzz harness in #1193.

Required GitHub-side checks remain unchanged. The differential fuzz
workflow is informational; failure does not block merge unless the
PR carries the `fuzz-required` label, in which case the maintainer
has explicitly opted in.

The workflow installs upstream rsync 3.4.1 from the Debian-based
`rsync-test` container (referenced in MEMORY.md -> CI/Interop Notes)
to guarantee a known-good baseline.

## 12. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Filename encoding divergence (UTF-8 source, Latin-1 child stdout) | Set `LANG=C.UTF-8` for both children; canonicalise paths to UTF-8 NFC before set comparison; reject candidate filenames whose Latin-1 round-trip differs. |
| Platform fnmatch differences (BSD libc on macOS vs glibc on Linux) | Both binaries use rsync's bundled `wildmatch.c` rather than libc `fnmatch(3)`; the oracle compares two implementations of *the same* algorithm. Pin the harness to Linux musl in CI to remove macOS as a confounder. |
| Locale-dependent character classes (`[A-Z]` includes `[` on POSIX C locale) | Set `LC_ALL=C` for both children. Avoid character-class seeds that span locale-sensitive ranges. |
| Tempdir path leakage across iterations | Each iteration creates a fresh `tempfile::TempDir` with `Drop`-cleanup; the harness does NOT use long-lived shared state. |
| Test flakiness from quick-check skip | Force `--no-times` (already in section 4) and use distinct file sizes for any fixture that depends on per-file decisions. Project memory confirms this pattern (MEMORY.md -> Test Flakiness). |
| Symlink loops in generated trees | The tree generator forbids cycles by construction: symlinks always target an absolute path under a sibling temp tree, never within the current source tree. |
| Upstream rsync absent from runner | The workflow declares the rsync 3.4.1 install as a hard prerequisite; absence aborts the run with a clear error rather than silently passing. |
| Clock skew | Not applicable. `--dry-run` does not consult the clock for filter decisions. |
| Resource exhaustion via pathological globs | The 256-byte rule line cap and the 10,000-entry tree cap bound globset compilation cost; libfuzzer's per-input timeout (`-timeout=10`) reaps anything slower. |

## 13. Interaction with #1062

The path-escaping CVE audit in #1062 produced a list of inputs that
must remain confined to the transfer root. Those inputs become the
*highest-priority seed corpus entries* for this harness:

- They are the most likely to surface a divergence with security
  impact.
- They are the cheapest to evaluate (single-file trees).
- They cleanly differentiate "filter elides path" from "evaluator
  panics or escapes".

The seed importer reads `crates/filters/fuzz/corpus/filter_parse/` and
the path-escaping seed list from #1062's audit attachment, deduplicates
on rule-set hash, and writes the merged set into
`crates/filters/fuzz/corpus/filter_differential/`. The existing 17
parse-corpus entries listed in section 1's enumeration carry over
without modification.

## 14. Tracking (follow-up TODOs, not added to the persistent list)

The implementation work breaks into four merge-sized PRs:

1. **Implement `fuzz_filter_differential` target** -- add the new
   `[[bin]]` entry to `crates/filters/fuzz/Cargo.toml`, write the
   target file, derive `Arbitrary` impls for `FuzzInput`, wire up
   `tempfile`-based tree materialisation and child-process spawn.
2. **Seed the corpus from #1292 and #1062** -- copy the existing
   filter parse/chain corpora, add the path-escaping seed list,
   add the trailing-slash, `**` versus `*`, and re-include canonical
   recipes from section 5.
3. **Add the daemon-server differential mode** -- second target
   (or runtime-mode flag on the first target) that spawns
   `oc-rsyncd` and upstream `rsyncd` against a generated
   `rsyncd.conf`. Reuses `scripts/rsync-interop-server.sh`.
4. **Add the opt-in CI workflow and regression-test extraction** --
   `.github/workflows/fuzz-filter-differential.yml`, the
   `cargo fuzz tmin` post-processor, and the artefact uploader.
   Document the `fuzz-required` label policy in `CONTRIBUTING.md`.

Each follow-up references this design note. None modify production
code under `crates/filters/src/` beyond the public re-exports already
in `lib.rs`. The harness is purely additive.

## 15. Open questions

- Should the daemon-mode harness compare both push and pull? Push
  filtering goes through `crates/transfer/src/generator/filters.rs`
  and pull filtering goes through the daemon's filter list before
  the file list is sent. Both paths matter; recommendation is to
  cover both in the daemon harness with a 50/50 split.
- Is `--filter='dir-merge .gitignore'` worth a dedicated mode? The
  `gitignore`-style anchoring (anchor at the merge-file directory,
  not the transfer root) is a non-trivial deviation handled by
  `crates/filters/src/chain.rs`. Recommendation: yes, but as a
  subsequent PR after the basic harness is green.
- Should clear directives (`!`) interact with merge-file scope?
  Upstream limits `!` to the current scope; a clear inside a
  `dir-merge` does not propagate up. The harness must seed both the
  scoped and unscoped variants explicitly, since the random grammar
  is unlikely to hit the boundary by chance.

## 16. Success criteria

The harness is considered production-ready when:

- 100,000 nightly iterations have run with zero divergences for one
  full week against rsync 3.0.9, 3.1.3, and 3.4.1.
- Every input in `crates/filters/fuzz/corpus/filter_differential/`
  is replayed deterministically with both implementations producing
  identical output.
- The opt-in CI workflow has been triggered and passed on at least
  five PRs that touch `crates/filters/src/` or
  `crates/daemon/src/daemon/sections/module_access/`.
- A documented divergence-to-regression-test pipeline exists, with
  at least one historical divergence (replayed from #1062 or #1696)
  converted into a permanent nextest case under
  `crates/filters/tests/`.

These criteria mirror the post-merge gates established for the
protocol fuzz harness (#1193) and ensure parity of confidence
between the two fuzzing surfaces.
