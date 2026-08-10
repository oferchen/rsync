# zsync Optimizations: CHANGELOG and Release Notes (#2087)

## 1. Background

The zsync-inspired matching work introduces four wire-compatible internal
speedups to the receiver's block-match path. Each PR is purely an internal
refactor: hash layout, pre-filter, run-extension, and post-match pruning.
None of them alters wire bytes, capability flags, or sum-head fields. They
remain invisible to upstream rsync and to any non-oc-rsync peer.

The four PR clusters:

- **bithash prefilter** (#2059, #2060, #2061, #2062): cheap 32-bit bitmap
  test before the strong-checksum lookup, eliminating the hot-path hashtable
  miss for non-matching blocks.
- **seq-match extend-run** (#2064, #2065, #2066): after a successful match,
  attempt sequential block matches starting at `block_index + 1` before
  re-entering the rolling-hash loop.
- **matched-block pruning** (#2068, #2069, #2070): once a basis block is
  matched, remove it from the lookup table so future probes skip it.
- **compact-keys layout** (#2072, #2073): pack `(weak_hash, block_index)`
  into a single cache-line-aligned array for sequential probes.

## 2. CHANGELOG entries

Add to `CHANGELOG.md` under the next release's `### Performance` section:

```
- perf(matching): bithash prefilter cuts strong-hash misses on the receiver
  hot path (#2059, #2060, #2061, #2062).
- perf(matching): extend-run after a match consumes sequential basis blocks
  without re-entering the rolling-hash loop (#2064, #2065, #2066).
- perf(matching): prune matched basis blocks from the lookup table to skip
  duplicate probes on later windows (#2068, #2069, #2070).
- perf(matching): compact-keys layout packs weak-hash plus block index for
  cache-friendly sequential probes (#2072, #2073).
```

## 3. Release notes block

Place under the `Performance` heading in the GitHub release body:

> **Receiver matching, internal-only.** The block-match scheduler now runs a
> 32-bit bithash prefilter before each strong-checksum lookup, falls through
> to a sequential extend-run after a hit, drops matched basis blocks from
> the lookup table, and stores keys in a compact `(weak_hash, block_index)`
> array. These changes reduce CPU on the receiver for typical delta
> workloads. They are entirely internal: wire bytes, capability strings,
> sum-head fields, and protocol negotiation are unchanged. Transfers to and
> from upstream rsync 3.0.9, 3.1.3, and 3.4.1 are byte-identical to prior
> oc-rsync releases.

## 4. Anti-spec

These optimizations MUST NOT be advertised as wire features:

- No new capability bit, flag, or sum-head field.
- No new env var, CLI flag, or daemon option exposed to users.
- No mention in `--help`, man pages, or protocol documentation.
- Compatibility matrix and golden wire tests are unchanged.
- Interop tests against upstream 3.0.9 / 3.1.3 / 3.4.1 must continue to
  pass byte-for-byte.

If a future change to any of these four mechanisms would alter wire bytes,
treat it as a wire-protocol change and follow the full protocol-extension
review process.

## 5. Benchmark numbers (placeholder)

Final receiver-CPU and wall-clock numbers are pending the benchmark sweeps
tracked in #2081 (synthetic delta workloads) and #2082 (real-world repo
mirrors). Update this section and the release notes block once those PRs
land. Expected sections:

- Receiver CPU vs prior release (single-file, 1 GiB, 50% delta).
- Receiver CPU vs upstream 3.4.1 on the same workload.
- Wall-clock for the daemon mirror benchmark (`scripts/benchmark.sh`).
- Hash-table probe count from the matching microbench.
