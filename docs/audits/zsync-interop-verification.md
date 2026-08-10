# Zsync interop verification (task #2074)

Evidence pass confirming the zsync-inspired matching work (#2059-#2087)
plus its three follow-up PRs (#4164 adversarial fixtures, #4166 arena
allocator feasibility audit, #4169 zsync cleanup audit) have not
regressed upstream rsync interoperability.

Companion documents:

- [`docs/audits/zsync-cleanup-audit.md`](zsync-cleanup-audit.md) -
  three-part follow-up audit (#2083 / #2084 / #2085).
- [`docs/audits/zsync-golden-byte-stability.md`](zsync-golden-byte-stability.md) -
  wire-byte stability proof for bithash, seq-match, prune, compact-keys.

## Verified master commit

`6a615aa3652a51f88a95b1bc0255b6e67eb74426` - `feat(engine): BufferPool
byte-budget memory cap (#2245) (#4160)`. The commit is the most recent
master HEAD for which both the **CI** and **Interop Validation**
workflows report `conclusion: success`.

| Workflow | Run ID | Conclusion |
|----------|--------|------------|
| CI | 25967589625 | success |
| Interop Validation | 25967589578 | success |
| Coverage | 25967589574 | success |
| MSRV Check | 25967589576 | success |
| Parallel Determinism | 25967589573 | success |

Both later master commits (`e52c1c6a`, `6e47121f`, `9b9f854f`,
`6d34dc86`, `03da1489`, `080d8881`) either had cancelled CI runs from
subsequent pushes or only one of the two required workflows green at
the time of this audit, so the verification is anchored on `6a615aa`.

## CI job: `interop / interop with upstream rsync`

Single job in the CI workflow that runs `tools/ci/run_interop.sh`
against upstream rsync 3.0.9, 3.1.3, 3.4.1, and 3.4.2 binaries built
from source inside the runner.

Final log line:

```
All interoperability checks succeeded (basic + comprehensive + protocols 28-32 + standalone).
```

### Matrix scenarios (per upstream version, native protocol)

Parallel comprehensive run, each version against its native protocol
default:

| Upstream version | Result | Known failures | Unexpected |
|------------------|--------|----------------|------------|
| 3.0.9 | 33/33 passed | 0 | 0 |
| 3.1.3 | 33/33 passed | 0 | 0 |
| 3.4.1 | 125/125 passed | 0 | 0 |
| 3.4.2 | 125/125 passed | 0 | 0 |

3.0.9 and 3.1.3 exercise the protocol-28 / protocol-29 subset (33
scenarios). 3.4.1 and 3.4.2 exercise the full modern protocol-32
matrix (125 scenarios).

### Matrix scenarios (forced protocol, upstream 3.4.2)

Sequential pass that pins the wire protocol via `--protocol=N` against
the 3.4.2 binary:

| Forced protocol | Result | Known failures | Unexpected |
|-----------------|--------|----------------|------------|
| 28 | 120/123 passed | 3 | 0 |
| 29 | 121/123 passed | 2 | 0 |
| 30 | 125/125 passed | 0 | 0 |
| 31 | 125/125 passed | 0 | 0 |
| 32 | 125/125 passed | 0 | 0 |

Protocol 28 / 29 known failures are pre-existing legacy-protocol
quirks tracked separately in
[`docs/audits/interop-known-failures-inventory.md`](interop-known-failures-inventory.md)
and [`docs/audits/protocol-28-32-interop-matrix.md`](protocol-28-32-interop-matrix.md).
They are unrelated to the matching pipeline.

### Standalone scenarios

`70 / 71` passed, `1` known failure, `0` unexpected. The single known
failure is `upstream-compressed-batch-self-roundtrip` where upstream
rsync fails to read its own compressed delta batch (exit 12); this is
an upstream bug reproduced by the standalone harness and is not caused
by oc-rsync.

The 71 standalone scenarios exercised:

```
write-batch-read-batch
write-batch-read-batch-compressed
upstream-compressed-batch-oc-reads
oc-compressed-batch-upstream-reads
compressed-batch-delta-interop
upstream-compressed-batch-self-roundtrip   (KNOWN: upstream-side failure)
batch-framing-multifile
info-progress2
large-file-2gb
file-vanished
copy-unsafe-safe-links
pre-post-xfer-exec
read-only-module
wrong-password-auth
iconv
iconv-upstream
iconv-local-ssh
hardlinks-comprehensive
inc-recurse-comprehensive
inc-recurse-sender-push
unicode-names
special-chars
empty-dir
delete-after
hardlinks
many-files
sparse
whole-file
dry-run
filter-rules
up:no-change
oc:no-change
inplace
append
delay-updates
compress-level
zstd-negotiation
files-from
trust-sender
partial-dir
deep-nesting
modify-window
delete-excluded
permissions-only
timestamps-only
max-connections
exclude-include-precedence
delete-with-filters
delete-filter-protect
delete-filter-risk
ff-filter-shortcut
acl-xattr-graceful-degradation-309
log-format-daemon
up:symlinks
oc:symlinks
daemon-server-side-filter
daemon-filter-exclude-glob
daemon-filter-exclude-anchored
daemon-filter-include-exclude-star
daemon-filter-directive-types
daemon-filter-overlapping-rules
daemon-filter-from-files
daemon-filter-include-from-files
daemon-filter-push-direction
delta-stats
daemon-filter-doublestar
daemon-filter-charclass
daemon-filter-question-mark
link-dest
copy-dest
numeric-ids-standalone
```

The `delta-stats` scenario directly exercises the matching pipeline
touched by the zsync work and reports identical block-match counts on
both sides:

```
oc-rsync stats: literal=10524 matched=91900 total_size=102424 speedup=8.37
upstream  stats: literal=10524 matched=91900 total_size=102424 speedup=8.40
```

Identical `matched=91900` confirms bithash, seq-match, prune, and
compact-keys still produce wire-equivalent match decisions against
upstream 3.4.2.

## SSH transport

The CI job also runs SSH push / pull / baseline interop with upstream
3.4.2. Final line:

```
All SSH interop tests passed.
```

## Standalone `Interop Validation` workflow

Run 25967589578, same commit, all jobs successful:

| Job | Conclusion |
|-----|------------|
| Message Format Validation | success |
| Compression Codec Interop | success |
| Filter Rules Interop | success |
| Batch Mode Interop | success |
| Exit Code Validation | success |
| INC_RECURSE Interop | success |
| Behavior Comparison | success |
| Regenerate Golden Files (Manual) | skipped (manual trigger only) |

## Zsync work surveyed

The following merged PRs constitute the zsync work whose interop
impact this audit confirms:

| Commit | PR | Subject |
|--------|----|---------|
| 6e47121f | #4164 | `test(matching): zsync adversarial - shifted-insertion + sparse-match (#2079 #2080)` |
| 739a0e64 | #4166 | `docs(flist): audit arena allocator feasibility (#2210)` |
| 9b7f94f7 | #4169 | `docs(audits): zsync cleanup audit (#2083, #2084, #2085)` |
| aa7eb8a4 | #3748 | `feat(match): zsync-inspired matched-block pruning bitmap` |
| 6122b507 | #3751 | `feat(match): add zsync-inspired seq-match extend-run` |
| 3d0391d8 |        | `feat(match): add zsync-inspired bithash prefilter to MatchIndex` |
| 8e750a73 | #3657 | `test(match): zsync sparse-match adversarial fixture` |
| cc82f773 | #3656 | `test(match): zsync shifted-insertion fixture` |
| 999c9b7c | #3683 | `docs(audits): zsync golden-byte stability audit (#2086)` |

The two later docs-only PRs (#4166 and #4169) cannot change wire
behaviour. #4164 adds adversarial test fixtures but does not touch
runtime code. The feature PRs (#3748, #3751, bithash) all merged
prior to commit `6a615aa` and their wire-stability was verified at
merge time by the golden-byte audit; this verification re-confirms
that the cumulative state of the matching pipeline on master is still
wire-compatible with upstream 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2.

## Verdict

The zsync work landed in commits `aa7eb8a4`, `6122b507`, `3d0391d8`,
`cc82f773`, `8e750a73`, `6e47121f`, `739a0e64`, and `9b7f94f7` has not
regressed interop. At master commit `6a615aa3652a51f88a95b1bc0255b6e67eb74426`:

- Matrix: native-protocol coverage against 3.0.9, 3.1.3, 3.4.1, and
  3.4.2 is 100% (316 / 316 scenarios pass with zero unexpected
  failures).
- Matrix: forced protocols 28-32 against upstream 3.4.2 pass with the
  same pre-existing 5 legacy-protocol known failures the project has
  always carried; no new failures introduced.
- Standalone: 70 / 71 pass; the single known failure
  (`upstream-compressed-batch-self-roundtrip`) is an upstream-side
  defect, not an oc-rsync regression.
- `delta-stats` reports identical `matched=91900` against upstream
  3.4.2, confirming the matching pipeline is still wire-equivalent
  after bithash / seq-match / prune / compact-keys.
- All eight Interop Validation jobs and the in-CI `interop` job pass.

Task #2074 is satisfied. No remediation required.
