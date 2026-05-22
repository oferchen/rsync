# Landlock outcome log format reference for operators

Audience: sysadmins and SREs operating fleets of `oc-rsyncd` instances who need to grep, parse, or alert on the Landlock LSM enforcement state reported per connection.

The daemon emits exactly one Landlock outcome line per accepted connection, after the module is resolved and any chroot / uid drop completes, immediately before user-controlled file operations begin. The line's machine-readable fields are stable for log-aggregation pipelines.

## Field schema

Every Landlock line carries the following keys, in this order:

| Key       | Required | Example value                | Notes |
|-----------|----------|------------------------------|-------|
| `landlock=`  | yes      | `fully_enforced`             | Status string. One of five values - see below. |
| `level=`     | partial  | `v1`, `v2`, `v3`             | Present only when `landlock=partially_enforced`. Reports the achieved ABI tier after best-effort downgrade. |
| `module=`    | yes      | `backups`                    | The `rsyncd.conf` module the connection is serving. Lets operators slice by module. |
| `peer=`      | yes      | `10.0.0.5:54321`             | `<ip>:<port>` of the client. IPv6 peers are bracketed: `[2001:db8::1]:54321`. |
| `pid=`       | yes      | `12345`                      | PID of the per-connection worker. Use to correlate with `oc-rsyncd` process tables. |
| `reason=`    | partial  | `"kernel<5.13 or LSM disabled"` | Present on `unavailable` and `error` lines. Quoted free-form string for human triage. |

Severity is logged via the tracing level (`INFO`, `WARN`, `ERROR`). Log sinks that strip levels can still distinguish outcomes via the `landlock=` value alone.

## Status field values

The five valid values for `landlock=`:

- `landlock=fully_enforced` - kernel 6.2+, full ABI v3 ruleset honoured (READ / WRITE / CREATE / DELETE / RENAME / SYMLINK / REFER / TRUNCATE). This is the target posture.
- `landlock=partially_enforced` - kernel accepted the ruleset but downgraded some rights via best-effort. Typically 5.13 - 5.18 (no REFER, no TRUNCATE) or 5.19 - 6.1 (no TRUNCATE). The sandbox is active for the rights the kernel honoured; the achieved tier is reported in the `level=` field.
- `landlock=not_enforced` - kernel accepted the ruleset but applied nothing (best-effort downgrade returned an empty effective ruleset). Equivalent to no sandbox; the SEC-1 `*at` syscall chain is the sole defense.
- `landlock=unavailable` - pre-5.13 kernel or LSM disabled at boot. Same posture as `not_enforced`, but distinguishable in logs so operators can alert on kernel age separately from configuration faults.
- `landlock=error: "<io::Error message>"` - ruleset construction or `restrict_self()` failed even though the kernel advertised support. The daemon treats this as a fatal connection error and aborts the session. Investigate immediately - the intended sandbox did not engage and the connection was refused.

## Example log lines

Copy-pasteable samples for grep / regex testing:

```text
INFO landlock=fully_enforced module=backups peer=10.0.0.5:54321 pid=12345
INFO landlock=partially_enforced level=v2 module=backups peer=10.0.0.5:54322 pid=12346
INFO landlock=partially_enforced level=v1 module=backups peer=10.0.0.5:54323 pid=12347
WARN landlock=not_enforced module=backups peer=10.0.0.5:54324 pid=12348 reason="best-effort downgrade returned empty ruleset"
WARN landlock=unavailable module=backups peer=10.0.0.5:54325 pid=12349 reason="kernel<5.13 or LSM disabled"
ERROR landlock=error: "Permission denied (os error 13)" module=backups peer=10.0.0.5:54326 pid=12350
```

IPv6 example:

```text
INFO landlock=fully_enforced module=backups peer=[2001:db8::1]:54321 pid=12351
```

## Operator alerting recipes

Concrete alerts to wire into Prometheus, Loki, Splunk, or any text-grep pipeline:

- **Page on `landlock=error`.** Treat as a fatal connection error. Indicates the kernel exposed Landlock but the daemon could not engage the ruleset. Investigate the daemon's effective capabilities, AppArmor / SELinux interactions, and recent kernel updates. Connections affected were refused; clients see an immediate disconnect.
- **Track `landlock=unavailable` rate across the fleet.** Indicates hosts running kernels older than 5.13 or with the LSM disabled at boot (`CONFIG_LSM` missing `landlock` or `lsm=` kernel cmdline excluding it). Should trend to zero as the fleet upgrades to 5.13+. A sudden spike on hosts that previously reported enforcement points to a kernel rollback or boot-parameter regression.
- **Track `landlock=not_enforced` rate.** Distinct from `unavailable` - the kernel exposes Landlock but the ruleset evaluated to nothing. Usually a configuration bug (empty `module.path`, missing roots). Should be zero in steady state.
- **Track `landlock=partially_enforced level=v1` ratio.** Indicates kernels below 5.19; REFER is not enforced (cross-tree renames are unguarded by Landlock but still covered by the SEC-1.j `renameat` helper). Acceptable on long-lived LTS hosts; flag if it appears on hosts you expect to be 5.19+.
- **Track `landlock=partially_enforced level=v2` ratio.** Indicates kernels 5.19 - 6.1; TRUNCATE not enforced. Acceptable for steady-state stable distros; should drop as hosts move to 6.2+.
- **Confirm `landlock=fully_enforced` for >95% of connections.** Confirms the fleet is on 6.2+ with the full v3 ruleset engaged. Use as a release-gate SLO when standardising on a new distro baseline.

## Aggregation one-liners

Single-host histogram of recent outcomes:

```sh
grep 'landlock=' /var/log/oc-rsyncd.log \
  | sed -n 's/.*landlock=\([a-z_]*\).*/\1/p' \
  | sort | uniq -c
```

Per-module breakdown:

```sh
grep 'landlock=' /var/log/oc-rsyncd.log \
  | sed -n 's/.*landlock=\([a-z_]*\).*module=\([^ ]*\).*/\1 \2/p' \
  | sort | uniq -c
```

Loki / Promtail (LogQL):

```text
# error rate over 5 minutes - page when non-zero
rate({app="oc-rsyncd"} |= "landlock=error" [5m])

# share of connections fully enforced over 1 hour - SLO query
sum(rate({app="oc-rsyncd"} |= "landlock=fully_enforced" [1h]))
  / sum(rate({app="oc-rsyncd"} |~ "landlock=" [1h]))
```

Splunk:

```text
index=oc_rsyncd "landlock=error"
  | stats count by host
```

Prometheus exporter regex (for a generic log exporter such as `mtail` or `grok_exporter`):

```text
landlock=(?P<status>[a-z_]+)(?: level=(?P<level>v[0-9]+))?
  .* module=(?P<module>\S+) peer=(?P<peer>\S+) pid=(?P<pid>\d+)
```

## Cross-references

- `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` - canonical kernel-version-to-ABI matrix and threat model.
- `docs/packaging/landlock-feature-guidance.md` - distro packaging guidance and runtime behaviour matrix.
