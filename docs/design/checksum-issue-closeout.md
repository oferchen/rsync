# Checksum Issue #970 Close-out Criteria (CSM-9.b)

Tracking: CSM-9.b. Parent: CSM-9. Series: CSM-1 through CSM-9.
Depends: CSM-8 (PR #5128), CSM-9.a (PR #5260).
Status: planned.

## 1. Issue History

### 1.1 Original report

Issue #970 reported a 1.5-1.7x wall-clock regression in `--checksum`
mode compared to upstream rsync 3.4.1. The gap was consistent across
file sizes and reproducible on Linux x86_64 bare metal.

### 1.2 Investigation timeline

| Phase | Task | Finding |
|-------|------|---------|
| CSM-1 | Initial measurement | 1.5-1.7x upstream across all fixtures |
| CSM-2.a | Flamegraph profiling | MD5 consuming 40-60% of total CPU |
| CSM-3 | Syscall comparison | 2.04x read calls, 3.34x stat calls |
| CSM-8 | Root cause fix | Checksum negotiation selected MD5 instead of XXH3/128 |
| CSM-9.a | Rebench methodology | Defined 4-tier fixture suite and measurement protocol |

### 1.3 Root cause

The checksum negotiation code path had a logic error: when both sides
advertised XXH3 support via the capability string, oc-rsync fell
through to the MD5 fallback path instead of selecting XXH3-128. This
caused a ~50x per-byte throughput penalty (MD5 at ~600 MB/s vs XXH3
at ~30 GB/s on modern x86). CSM-8 (PR #5128) corrected the selection
logic so XXH3-128 is used when both endpoints support it.

### 1.4 Residual contributors

After CSM-8, the algorithm penalty is eliminated. Known residual
contributors to any remaining gap:

- Per-file stat overhead (STX-8 cached flist metadata - merged).
- BufReader EOF probes adding spurious read syscalls (STX-6 pre-sized
  reads - merged).
- Read buffer size differences (64 KB vs upstream 256 KB).

## 2. Acceptance Criteria

### 2.1 Primary criterion

All four fixture tiers must show oc-rsync within **1.05x** of upstream
rsync 3.4.1 wall-clock time (median over 10 runs):

| Fixture | File size | File count | Total data | Target |
|---------|-----------|------------|------------|--------|
| tiny | 1 KB | 10,000 | ~10 MB | <= 1.05x |
| medium | 1 MB | 1,000 | ~1 GB | <= 1.05x |
| large | 100 MB | 10 | ~1 GB | <= 1.05x |
| huge | 1 GB | 1 | 1 GB | <= 1.05x |

### 2.2 Secondary criteria

| Criterion | Threshold | Purpose |
|-----------|-----------|---------|
| No individual run exceeds 1.10x | Hard cap | Guards against high-variance masking |
| XXH3 confirmed active | XXH3-128 in perf profile | Validates fix took effect |
| Syscall ratio (read) | <= 1.10x upstream | STX-6 effectiveness |
| Syscall ratio (stat) | <= 2.00x upstream | STX-8 effectiveness |

### 2.3 Measurement conditions

Per CSM-9.a methodology:

- Platform: Linux x86_64, bare metal or dedicated VM.
- CPU: fixed frequency, turbo boost disabled, `performance` governor.
- Filesystem: ext4 with `noatime`.
- Cache: dropped between runs (`echo 3 > /proc/sys/vm/drop_caches`).
- Tool: `hyperfine --warmup 3 --runs 10`.
- Binaries: oc-rsync release (LTO=thin) and upstream rsync 3.4.1 from
  source, built on same host.

## 3. Evidence Format

### 3.1 Required artifacts

The following must be posted as an issue comment on #970 before closure:

1. **Results table** - one row per fixture with median, mean, stddev,
   and ratio.
2. **Statistical summary** - confirmation that all fixtures pass the
   1.05x threshold with no individual run exceeding 1.10x.
3. **Before/after comparison** - pre-CSM-8 ratio (from CSM-1/2)
   alongside post-fix ratio.
4. **Environment record** - kernel version, CPU model, rustc version,
   upstream rsync version, commit hash of oc-rsync binary.

### 3.2 Results table format

```
## Checksum Mode Benchmark Results (CSM-9.b close-out)

Environment: [kernel, CPU, rustc, oc-rsync commit]
Upstream: rsync 3.4.1, built from source
Methodology: hyperfine --warmup 3 --runs 10, cache dropped between runs

| Fixture       | oc-rsync (s) | upstream (s) | Ratio | stddev oc | stddev up | Pass |
|---------------|-------------|-------------|-------|-----------|-----------|------|
| tiny (10Kx1K) | X.XXX       | X.XXX       | X.XXXx| X.XXX     | X.XXX     | Y/N  |
| medium (1Kx1M)| X.XXX       | X.XXX       | X.XXXx| X.XXX     | X.XXX     | Y/N  |
| large (10x100M)| X.XXX      | X.XXX       | X.XXXx| X.XXX     | X.XXX     | Y/N  |
| huge (1x1G)  | X.XXX       | X.XXX       | X.XXXx| X.XXX     | X.XXX     | Y/N  |

Before CSM-8: 1.5-1.7x (all fixtures). After: [values above].
```

### 3.3 Before/after comparison

Include a two-row summary showing the improvement:

| State | Tiny | Medium | Large | Huge |
|-------|------|--------|-------|------|
| Pre-CSM-8 (MD5 fallback) | ~1.6x | ~1.5x | ~1.7x | ~1.6x |
| Post-fix (XXH3 + STX-6/8) | TBD | TBD | TBD | TBD |

## 4. Partial Pass Handling

### 4.1 Definition

A partial pass occurs when 3 of 4 fixtures meet the 1.05x target but
one does not.

### 4.2 Procedure

1. **Do not close issue #970.** A partial pass is not a close.
2. **Document the passing fixtures.** Post results as an issue comment
   noting which fixtures pass and which fail.
3. **Profile the failing fixture.** Run `perf record -g` and generate
   a flamegraph. Identify the dominant contributor to the residual gap.
4. **File a follow-up issue.** Title: `perf: --checksum mode residual
   gap on [fixture] ([contributor])`. Link to #970. Include the
   flamegraph and identified hot path.
5. **Update the issue comment** with a link to the follow-up and the
   expected fix path.

### 4.3 Expected failure modes

| Failing fixture | Likely cause | Fix path |
|-----------------|-------------|----------|
| tiny only | Per-file open/close/stat overhead | Pool FDs, batch stat via io_uring |
| huge only | I/O bandwidth or read buffer size | Increase read buffer, consider mmap |
| tiny + medium | Per-file constant factor | Systematic overhead audit |
| large + huge | Compute or I/O streaming | Profile hash bandwidth vs I/O wait |

### 4.4 Re-close attempt

After the follow-up fix lands, re-run the full 4-fixture bench. If all
four pass, proceed with section 5 close-out. If another fixture fails,
repeat section 4.2.

## 5. Close-out Process

### 5.1 Steps

1. Run the CSM-9.a benchmark on Linux hardware with all fixes active
   (CSM-8 + STX-6 + STX-8).
2. Verify all four fixtures pass the 1.05x threshold (section 2.1).
3. Verify secondary criteria (section 2.2).
4. Format results per section 3.2 and post as issue comment on #970.
5. Add a final summary line:
   `Closing: all fixtures within 1.05x upstream. Root cause was MD5
   fallback (CSM-8 fix). Residual overhead addressed by STX-6/8.`
6. Close issue #970 with the comment link as evidence.
7. Update `docs/design/csm-bench-results.md` with the final results
   section.

### 5.2 Reviewability

The close-out comment must be self-contained - a reader should
understand the issue, fix, and evidence without following links. Include
the results table, the before/after comparison, and the environment
record in a single comment.

## 6. Interaction with STX-9/10

### 6.1 Relationship

The 1.5-1.7x gap has two independent contributors:

| Contributor | Fix | Expected contribution |
|-------------|-----|---------------------|
| Algorithm mismatch (MD5 vs XXH3) | CSM-8 | Dominant (40-60% CPU) |
| Syscall overhead (extra stat + read) | STX-6, STX-8 | Secondary (~30% of remaining gap) |

Both fix chains are needed together to reach 1.05x. CSM-8 alone brought
the ratio to approximately 2.0-2.9x (algorithm fixed but I/O overhead
remains). STX-6/8 address the remaining syscall overhead.

### 6.2 Combined validation

STX-10 (checksum wall-clock rebench) measures the combined effect of
all three fixes. CSM-9.b close-out uses the same data - if STX-10
passes, CSM-9.b close-out proceeds. They share:

- Same fixture corpus (section 2.1).
- Same pass threshold (1.05x).
- Same measurement methodology.

### 6.3 Failure attribution

If the combined bench fails:

1. Run CSM-9.a algorithm-only bench (CSM-8 contribution in isolation).
2. Run STX-9 strace comparison (syscall count reduction).
3. Compare: if algorithm bench passes (~1.0x for large/huge) but
   combined fails, the residual is in STX territory. If algorithm
   bench itself regresses, the CSM-8 fix may have been bypassed
   (e.g., capability string mismatch in the test environment).

### 6.4 Independence of closure

Issue #970 can close when the combined result passes, regardless of
whether the STX series has its own separate close-out. The STX series
tracks a broader scope (all modes, not just `--checksum`), while #970
is specifically about `--checksum` mode performance.

## 7. Timeline

### 7.1 Blocking dependency

CSM-9.b close-out is blocked on running the CSM-9.a benchmark on Linux
hardware. The methodology is defined (CSM-9.a, PR #5260) and the fixes
are merged (CSM-8, STX-6, STX-8). The remaining step is execution.

### 7.2 Execution prerequisites

| Prerequisite | Status |
|--------------|--------|
| CSM-8 merged | Done (PR #5128) |
| STX-6 merged | Done |
| STX-8 merged | Done |
| CSM-9.a methodology defined | Done (PR #5260) |
| Linux bench host available | Pending |
| oc-rsync release build at tip | Build on bench day |
| Upstream rsync 3.4.1 built | Build on bench day |

### 7.3 Expected sequence

1. Secure Linux bench host (bare metal or dedicated VM).
2. Build both binaries on that host.
3. Generate fixtures per CSM-9.a section 1.3.
4. Run hyperfine suite (4 fixtures, ~30 min total).
5. Evaluate pass/fail (same day).
6. If pass: post comment, close #970.
7. If partial pass: profile, file follow-up, leave #970 open.

### 7.4 Estimated completion

1-2 days from bench host availability. The bench itself takes under an
hour; the blocking factor is access to a properly isolated Linux host
with fixed CPU frequency.
