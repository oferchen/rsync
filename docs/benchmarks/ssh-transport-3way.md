# SSH transport 3-way benchmark

The release benchmark suite (`.github/scripts/benchmark.py`) now compares
three SSH transports side by side for every SSH workload:

1. **Upstream rsync 3.4.1 over OpenSSH subprocess** - reference baseline.
   Invoked with `host:path` operands, lets rsync fork `ssh` exactly as a
   user would on the command line.
2. **oc-rsync over OpenSSH subprocess** - same `host:path` operands, same
   external `ssh` binary, but the rsync implementation is oc-rsync.
   Isolates rsync-side cost from SSH-side cost.
3. **oc-rsync over embedded russh** - `ssh://host/path` URI operands route
   the transfer through the in-process russh client built into the
   `embedded-ssh` feature. Skips the `ssh` subprocess entirely.

The 2-bar `oc-rsync subprocess vs russh` view is preserved for backwards
compatibility (legacy fields `upstream`, `oc_rsync`, `ratio` in the JSON;
2-bar render in the chart and report). The 3-way view adds three fields
to each `ssh_transport` row in `benchmark_results.json`:

| Field | Meaning |
|-------|---------|
| `upstream_ssh` | timing for upstream rsync over OpenSSH subprocess |
| `oc_subprocess` | timing for oc-rsync over OpenSSH subprocess |
| `oc_russh` | timing for oc-rsync over embedded russh |
| `ratio_sub_vs_upstream` | `oc_subprocess.mean / upstream_ssh.mean` |
| `ratio_russh_vs_sub` | `oc_russh.mean / oc_subprocess.mean` |

## What to read from it

- **oc-sub / upstream** answers: "how does oc-rsync's per-file and per-block
  work compare to upstream rsync, with the SSH layer held constant?"
  A ratio below 1.0 means oc-rsync is faster than upstream rsync over the
  same OpenSSH pipe. This isolates the engine / protocol / I/O work from
  any SSH transport differences.
- **russh / oc-sub** answers: "what does the embedded SSH transport cost
  or save versus shelling out to OpenSSH?" A ratio below 1.0 means the
  in-process russh transport beats fork/exec-ing `ssh`. Both legs use the
  same oc-rsync engine, so this isolates SSH transport overhead from
  rsync work.

If both ratios are below 1.0, oc-rsync is faster than upstream at both
layers (engine and transport). If `oc-sub / upstream` is at parity and
`russh / oc-sub` is below 1.0, the savings come purely from skipping
`ssh` fork/exec, framing, and pipe overhead. If `russh / oc-sub` is
above 1.0, the embedded russh transport is paying a measurable cost
versus OpenSSH (cipher implementation, lack of `splice`, etc.) and may
warrant further profiling.

## Gating

The 3-way comparison only runs when:

- `OC_RSYNC_RUSSH` points to a binary built with the `embedded-ssh`
  feature, and
- the binary exists on disk.

The CI workflow `.github/workflows/benchmark.yml` already builds an
`oc-rsync-russh` binary for this purpose. The 3-way path is otherwise
skipped, so local runs without `embedded-ssh` are unaffected.

## Republishing the chart for an existing release

To regenerate the chart for an already-tagged release with the new
comparison, dispatch the benchmark workflow against that tag:

```sh
gh workflow run benchmark.yml -f target_tag=v0.6.2
```

The workflow rebuilds all three binaries, runs the benchmark, and
overwrites `benchmark.svg` / `benchmark.png` attached to the release.
