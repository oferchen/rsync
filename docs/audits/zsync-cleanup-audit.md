# Zsync cleanup audit (tasks #2083, #2084, #2085)

Combined verification pass covering three follow-ups to the zsync-inspired
matching work (#2059-#2087). Each section produces an explicit verdict
based on file-level evidence; this audit makes no code changes.

Companion documents:

- [`docs/audits/zsync-golden-byte-stability.md`](zsync-golden-byte-stability.md) -
  wire-byte stability proof for bithash, seq-match, prune, compact-keys.
- [`docs/design/zsync-inspired-matching.md`](../design/zsync-inspired-matching.md) -
  parent design note for the four optimizations.

## #2083 - benchmark script safety

**Verdict: PASS.**

### What was checked

Every shell script under `scripts/` that performs benchmark or profiling
work was inspected for the two failure modes the workspace has historically
suffered:

1. `rm -rf` with variable expansion that could escape into a host bind
   mount when run inside a container (the prior `/workspace` bind-mount
   loss incident the project documents internally).
2. Container `--setup` / `podman exec` invocations that pass `rm -rf`
   through multiple shell-quoting layers.

Files inspected:

- `scripts/benchmark.sh`
- `scripts/benchmark_100k.sh`
- `scripts/benchmark_1gb.sh`
- `scripts/benchmark_container.sh`
- `scripts/benchmark_flist_memory.sh`
- `scripts/benchmark_flist_memory_daemon.sh`
- `scripts/benchmark_hyperfine.sh`
- `scripts/benchmark_io_optimizations.sh`
- `scripts/benchmark_remote.sh`
- `scripts/benchmark_simple.sh`
- `scripts/benchmark_startup.sh`
- `scripts/bench-compare.sh`
- `scripts/bench_all_versions.sh`
- `scripts/profile_local.sh`
- `scripts/profile_transfer.sh`
- `scripts/profile_hotpaths.sh`
- `scripts/flamegraph_profile.sh`
- `scripts/run_arch_benchmark_container.sh`
- `scripts/run_full_benchmark_container.sh`

No zsync- or matching-specific benchmark scripts exist under `scripts/`
or `crates/matching/benches/`. The matching benches are Rust binaries
(`crates/matching/benches/delta_matching_benchmark.rs`,
`crates/matching/benches/profiling_analysis.rs`) and do not invoke
`rm -rf` against host paths.

### Evidence

Every script that issues `rm -rf` against a variable scratch path derives
that path from one of three safe roots:

| Pattern | Example | Safety |
|---------|---------|--------|
| `mktemp -d` | `scripts/benchmark_hyperfine.sh:147` (`workdir=$(mktemp -d)`); `scripts/benchmark_remote.sh:217`; `scripts/benchmark_100k.sh:293`; `scripts/benchmark_1gb.sh:202`; `scripts/benchmark_simple.sh:194`; `scripts/benchmark_startup.sh:69`; `scripts/bench-compare.sh:405`; `scripts/profile_transfer.sh:221`; `scripts/flamegraph_profile.sh:100` | Confined to `$TMPDIR` (typically `/tmp`); the OS-supplied unique suffix means no host bind-mount path can collide. |
| Hard-coded literal `/tmp/...` prefix | `scripts/profile_hotpaths.sh:20` (`BENCH_DIR="/tmp/rsync-profile"`); `scripts/benchmark_io_optimizations.sh:122` (`BENCH_DIR="/tmp/rsync-bench"`); `scripts/bench_all_versions.sh:40,63,83` (`/tmp/bench/dst`); `scripts/benchmark.sh:14` (`BENCHMARK_DIR="${BENCHMARK_DIR:-/tmp/rsync-benchmark}"`) | Literal `/tmp/...` prefix - no expansion can escape. |
| Path-guarded helper with allowlist | `scripts/benchmark_flist_memory.sh:119-152`; `scripts/benchmark_flist_memory_daemon.sh:134-160` | `check_prereqs` rejects any `BENCH_ROOT` not matching `/tmp/*` or `/var/tmp/*`; `safe_rm_under_root` refuses any path that does not begin with `$BENCH_ROOT/`. Comment at `benchmark_flist_memory.sh:141-142` explicitly cites the bind-mount disaster: *"Never use rm -rf with variable expansion outside this guard."* |

The single `--setup` invocation that crosses a hyperfine layer is
`scripts/benchmark_hyperfine.sh:139`:

```
hyperfine ... --setup "$setup_cmd" ...
```

`$setup_cmd` is built at lines 158, 177, 196, 215 from `"rm -rf $dest_up/* $dest_oc/*"`
where `dest_up=$workdir/dest_up` and `dest_oc=$workdir/dest_oc`. Because
`workdir=$(mktemp -d)` at line 147, the expanded `rm -rf` arguments are
always under `/tmp/tmp.<random>/`. The hyperfine docs forward the string
to `/bin/sh -c`, but the only variable it expands at execution time is
the literal mktemp path - no further shell substitution layer is added.

No `podman exec` invocation in any script passes `rm -rf` as part of the
command body. The two `podman exec` references in the tree are documentation
comments at `scripts/benchmark_flist_memory.sh:21` and
`scripts/benchmark_flist_memory_daemon.sh:37`, both invoking a script file
rather than an inline `rm`.

Container build/wrapper scripts (`benchmark_container.sh`,
`run_arch_benchmark_container.sh`, `run_full_benchmark_container.sh`) contain
zero `rm -rf` calls.

### Follow-ups

None. All scratch paths are either OS-randomised (`mktemp -d`), hard-coded
literal `/tmp` prefixes, or guarded by an allowlist that rejects anything
outside `/tmp` and `/var/tmp`. The mitigations documented after the
prior bind-mount incident are observably in force across the benchmark
surface.

## #2084 - no zsync CLI surface escape

**Verdict: PASS.**

### What was checked

The CLI and daemon configuration surface was searched for any user-facing
toggle that exposes a zsync-internal tunable. Per the design notes in
[`docs/design/zsync-inspired-matching.md`](../design/zsync-inspired-matching.md)
and [`docs/audits/zsync-golden-byte-stability.md`](zsync-golden-byte-stability.md),
the four zsync-inspired optimizations (bithash prefilter, seq-match
extend-run, matched-block pruning, compact-key layout) must remain purely
internal to `crates/matching/`. Exposing any of them as a CLI flag, daemon
config key, or environment variable would create an external promise the
project does not want to maintain and could perturb wire output.

Searched terms (case-insensitive, with both underscore and hyphen
variants): `bithash`, `seq_match` / `seqmatch`, `sparse_match` /
`sparsematch`, `compact_keys` / `compactkeys`, `zsync`, `prune` (filtered
to remove false matches against the upstream-standard `--prune-empty-dirs`
flag).

Directories scanned:

- `crates/cli/src/frontend/` - CLI argument parsing and help text.
- `crates/cli/src/` - full CLI crate.
- `crates/daemon/src/` - daemon config parser, session, connection pool.
- `crates/core/src/` - orchestration facade, config builders.

### Evidence

`grep -rln -iE 'bithash|seq[_-]?match|sparse[_-]?match|compact[_-]?keys|zsync' crates/cli/ crates/daemon/ crates/core/`
returns zero matches.

The only `prune` hits in `crates/cli/src/frontend/` are for the upstream
flag `--prune-empty-dirs` (e.g. `crates/cli/src/frontend/help.rs:115-116`)
which predates the zsync work and is independent of it.

The matching crate's public re-exports (`crates/matching/src/lib.rs:30-39`)
expose only the algorithm types (`DeltaGenerator`, `DeltaSignatureIndex`,
`DeltaScript`, `DeltaToken`, `FuzzyMatcher`) plus the
`HASH_KEY_BITS` / `HashtableRole` / `trace_*` symbols required by the
`--debug=HASH` parity work (#2187, commit `a311511da`). None of those are
tunables; they are read-only types and constants. The `bithash`,
`compact_lookup`, `matched_blocks`, and `builder` submodules of
`crates/matching/src/index/` are declared `mod`, not `pub mod` - their
internals are not reachable from outside the crate.

`crates/matching/Cargo.toml` declares one optional feature only,
`tracing`, which gates the existing logging instrumentation and has no
zsync-specific behaviour.

No environment-variable based knob exists either:
`grep -rn 'env::var\|std::env\|env_var' crates/matching/src/` returns
zero matches.

`crates/daemon/src/config.rs` (398 lines) contains no zsync term;
`oc-rsyncd.conf` parsing therefore cannot accept a zsync-tuning key.

### Follow-ups

None. The zsync optimizations are not externally observable through the
CLI, daemon config, or environment, exactly as the design called for.

## #2085 - no protocol crate change beyond internal API plumbing

**Verdict: PASS.**

### What was checked

Whether any zsync-related work landed in `crates/protocol/`, whether
through a renamed wire-format constant, a new public trait, or a
version-negotiated capability flag. The expected outcome (per
[`docs/audits/zsync-golden-byte-stability.md`](zsync-golden-byte-stability.md))
is that all zsync changes stay inside `crates/matching/` and that the
protocol crate sees nothing - not even a plumbing call.

Method:

1. Enumerate every commit on master that mentions a zsync term in its
   subject (`git log --oneline --all --grep='zsync\|bithash\|sparse'`).
2. For each commit, inspect `git show --stat` to confirm it touches no
   path under `crates/protocol/`.
3. Cross-check with a dependency probe: does `crates/protocol/Cargo.toml`
   depend on `matching`, and does any file in `crates/protocol/src/`
   import a `matching::` symbol?
4. Direct symbol search: any references to zsync-internal symbol names
   (`bithash`, `HASH_KEY_BITS`, `MatchedBlocks`, `MATCHED_BLOCKS`,
   `compact_keys`, etc.) inside `crates/protocol/`.

### Evidence

Zsync commits enumerated on master (subject lines truncated):

| Commit | Files touched |
|--------|---------------|
| `aa7eb8a45` zsync-inspired matched-block pruning bitmap (#3748) | `crates/match/src/generator.rs`, `crates/match/src/index/matched_blocks.rs`, `crates/match/src/index/matched_blocks_tests.rs`, `crates/match/src/index/mod.rs`, `crates/match/src/lib.rs` |
| `6122b5070` zsync-inspired seq-match extend-run (#3751) | `crates/match/src/generator.rs`, `crates/match/src/index/mod.rs`, `crates/match/src/index/tests.rs`, `crates/match/tests/*`, `crates/transfer/src/generator/{delta,tests,transfer}.rs` |
| `3d0391d80` zsync-inspired bithash prefilter to MatchIndex (#3737) | `Cargo.lock`, `crates/match/Cargo.toml`, `crates/match/src/index/{bithash,bithash_tests,builder,mod}.rs` |
| `a311511da` wire --debug=HASH producer emissions (#4141) | `crates/matching/src/index/{builder,compact_lookup,mod,trace}.rs`, `crates/matching/src/lib.rs`, `crates/matching/tests/debug_hash_emissions.rs`, `docs/audits/debug-flags-verbosity-matrix.md` |
| `411f189bb` zsync adversarial shifted-insertion + sparse-match (#2079/#2080) | `crates/matching/src/index/{mod,sparse_match_tests}.rs`, `crates/matching/tests/shifted_insertion_fixture.rs` |
| `cc82f7734` zsync shifted-insertion fixture (#3656) | `crates/match/tests/shifted_insertion_fixture.rs` |
| `8e750a737` zsync sparse-match adversarial fixture (#3657) | `crates/match/tests/sparse_match_fixture.rs` |

`git show --pretty=format: --name-only aa7eb8a45 6122b5070 3d0391d80 411f189bb a311511da | grep -i protocol` returns **zero** matches. The seq-match commit's
`crates/transfer/src/generator/delta.rs` adjustment is the only
non-matching/non-test touch across the whole zsync series, and it is
internal plumbing (passing `MatchedBlocks` through to
`find_match_slices_filtered`) confined to the `transfer` crate.

Dependency probe (`grep -rln 'matching::\|use matching' crates/protocol/`):
zero hits. `crates/protocol/Cargo.toml` does not list `matching` as a
dependency. The matching crate's test-only `dev-dependencies` include
`protocol` (so matching tests can build wire-format expectations), but
not the reverse - the protocol crate has no compile-time visibility into
the matching crate at all.

Symbol probe (`grep -rln -iE 'bithash|seq[_-]?match|sparse[_-]?match|compact[_-]?keys|zsync' crates/protocol/`):
zero hits. No zsync-internal token, constant, or struct name appears in
the protocol source tree.

Recent commits on `crates/protocol/` (`git log --oneline -30 -- crates/protocol/`)
cover unrelated work: `--debug=NSTR` plumbing (#2190), upstream
`print_child_argv` parity (#4152), debug-output wording fixes, comment
audits, and the `CF_INPLACE_PARTIAL_DIR` gating (#4064). None reference
zsync-inspired matching.

### Follow-ups

None. The protocol crate has no compile-time, run-time, or wire-level
exposure to zsync-internal state. The four optimizations remain
correctly isolated inside `crates/matching/` (with the seq-match plumbing
calling out to the matching API from `crates/transfer/`). The
golden-byte stability gap noted in
[`zsync-golden-byte-stability.md`](zsync-golden-byte-stability.md) for
the prune optimization is tracked separately and is not in scope here.

## Combined verdict

| Task | Verdict |
|------|---------|
| #2083 benchmark script safety | PASS |
| #2084 no zsync CLI surface escape | PASS |
| #2085 no protocol crate change beyond internal API plumbing | PASS |

All three checks are clean. No remediation work is required from this
audit. The zsync-inspired matching optimizations remain a purely
internal performance improvement with no externally observable surface.
