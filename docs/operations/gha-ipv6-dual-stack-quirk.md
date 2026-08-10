# GitHub Actions IPv6 dual-stack quirk

GitHub Actions hosted runners (Linux, Windows, macOS) ship with IPv6
partially configured: the kernel recognises the address family, the
`AF_INET6` socket type is available, but the runner is not given a
routable IPv6 address and `bind(2)` to `[::]:port` fails with
`EADDRNOTAVAIL` or, on a small subset of runner images, `EAFNOSUPPORT`.

This page documents how oc-rsync detects the situation, what it logs,
and when an operator or CI author needs to override the default. The
in-tree behaviour described here is the contract `oc-rsync` ships with;
the upstream rsync project independently exposes the same failure mode
through its own daemon test (see the
[upstream-testsuite cross-reference](#upstream-testsuite-cross-reference)
at the bottom of this page).

## 1. Why GitHub-hosted runners hit this

The default daemon listener startup follows upstream rsync's
`socket.c::open_socket_in` flow (rsync-3.4.1, lines 428-498): when no
explicit address family is requested, `getaddrinfo(NULL, port,
AI_PASSIVE, ...)` returns every available family and the listener
iterates over them, binding one socket per family.

On glibc that iteration returns IPv6 first, IPv4 second. GitHub
Actions runners expose:

- An IPv6 stack that `getaddrinfo` advertises (`::` is a valid
  `AI_PASSIVE` result).
- No routable IPv6 address bound to a runner-visible interface.
- A kernel `bind(2)` that consequently rejects `[::]:port` with
  `EADDRNOTAVAIL`.

Upstream rsync 3.4.1's `open_socket_in` logs the per-family bind
failure and continues to the next family. Pre-fix `oc-rsync` silently
swallowed the IPv6 failure, the IPv4 bind succeeded, the daemon
listened on IPv4 only, and a downstream upstream-testsuite assertion
that expected dual-stack behaviour exited with code 10. The opaque
exit was what triggered the investigation that this page documents.

## 2. How oc-rsync handles it on the default code path

Two source files in `crates/daemon/src/daemon/sections/server_runtime/`
implement the behaviour:

- `accept_loop.rs` builds the bind-address list. The default
  (no `address family` directive, no explicit `bind address`, no
  override env var) is dual-stack with `Ipv6Addr::UNSPECIFIED` first
  and `Ipv4Addr::UNSPECIFIED` second. This mirrors upstream
  `default_af_hint = 0` (`AF_UNSPEC`).
- `listener.rs::bind_listeners_per_family` walks that list and tries
  each family in turn. A per-family failure is logged as a warning
  through the daemon's log sink (or `stderr` if no log sink is
  configured) and the loop continues. The startup only fails when
  every family in the list fails to bind.

The warning emitted when IPv6 fails on a GHA runner looks like:

```
oc-rsyncd warning: IPv6 bind for [::]:873 failed: Cannot assign requested address (os error 99); continuing with remaining address families [daemon=<version>]
```

The IPv4 bind that follows succeeds, the daemon serves connections on
IPv4 only, and the warning is the diagnostic an operator sees that
explains why the listener is degraded.

The exact emission site is
`listener.rs::warn_per_family_bind_failure`. The site is gated to
"dual-stack startup" via the `dual_stack = bind_addresses.len() > 1`
check inside `bind_listeners_per_family`; single-family startups
(`address family = ipv4` or an explicit `bind address = 192.0.2.1`)
keep the original behaviour where the bind error propagates as a fatal
startup failure.

## 3. How to detect it in your own CI

If you run the `oc-rsync` daemon as part of a GitHub Actions job (or
any CI environment with the same partial-IPv6 shape), grep the daemon
log for the per-family warning string:

```yaml
- name: Surface daemon dual-stack warnings
  if: always()
  run: |
    if grep -q "IPv6 bind for \[::\]:.*failed:" oc-rsyncd.log; then
      echo "::warning::oc-rsyncd fell back to IPv4 because IPv6 bind was rejected"
    fi
```

The token `IPv6 bind for [::]:` is the canonical marker. The same
message body is emitted on `stderr` when the daemon runs without a
configured log file, so the same grep against the captured stderr of a
direct invocation works equally well.

A more targeted test, useful when reproducing the GHA shape on a local
machine that does have a routable IPv6 address, is to force the
listener into dual-stack mode and check that startup completes:

```sh
OC_RSYNC_DAEMON_ADDRESS_FAMILY=both oc-rsync --daemon --no-detach \
    --config=/path/to/oc-rsyncd.conf 2>&1 | grep "IPv6 bind for"
```

## 4. The workaround: defaults are correct, overrides are documented

The default behaviour described in section 2 is the workaround. CI
authors do not need to add IPv4-only configuration directives to keep
the daemon healthy on GitHub Actions runners; the dual-stack iteration
plus per-family warning handles the case transparently, and the IPv4
listener serves traffic exactly as it would on a dedicated host.

When the default is not what you want, three overrides are available:

| Mechanism | Effect | When to use |
| --- | --- | --- |
| `OC_RSYNC_DAEMON_ADDRESS_FAMILY=ipv4` env var | Bind one IPv4 listener only, never attempt IPv6. The bind error becomes fatal if IPv4 itself fails (no dual-stack fallback). | CI fixtures that want a clean log without the per-family warning, or that want to guarantee an IPv4 socket without enumerating IPv6 first. |
| `OC_RSYNC_DAEMON_ADDRESS_FAMILY=ipv6` env var | Bind one IPv6 listener only. The bind error is fatal if IPv6 fails. | Pure-IPv6 deployments where an IPv4 listener would be wasted or misleading. |
| `address family = ipv4` directive in `oc-rsyncd.conf`, or `--ipv4` on the daemon command line | Same effect as `OC_RSYNC_DAEMON_ADDRESS_FAMILY=ipv4`, but baked into the config rather than the environment. | Production deployments where the listener family is part of the persistent service definition. |

`OC_RSYNC_DAEMON_ADDRESS_FAMILY` accepts the case-insensitive values
`ipv4` / `v4` / `4` / `inet`, `ipv6` / `v6` / `6` / `inet6`, and
`both` / `dual` / `dualstack` / `dual-stack`. Unrecognised values are
ignored so an operator typo degrades to the compile-time default
rather than refusing to start. The variable is read once at
accept-loop entry; later changes do not affect a running daemon.

## 5. When to override

Override the default only when:

1. **You need a clean log on GHA.** The per-family warning is
   diagnostic, not actionable. Setting
   `OC_RSYNC_DAEMON_ADDRESS_FAMILY=ipv4` for the duration of a CI job
   suppresses it.
2. **You explicitly want IPv6-only.** GitHub Actions is not the target
   environment for this configuration; it is for production
   deployments behind a router that maps an IPv6 prefix to the
   daemon host.
3. **A regression test wants to force the GHA failure shape on a
   non-GHA host.** Set `OC_RSYNC_DAEMON_ADDRESS_FAMILY=both` on a
   machine without routable IPv6 to reproduce the per-family warning
   path deterministically.

Do not override the default to "fix" the warning on a normal
deployment. The warning is the contract: it tells the operator that
the dual-stack listener is degraded and the daemon is serving traffic
on the surviving family. Silencing it hides a legitimate signal.

## Upstream-testsuite cross-reference

The upstream rsync testsuite ships a `daemon` test whose assertions
expect a dual-stack listener. On GitHub Actions runners the assertion
fails because of the environmental quirk described above, not because
of an oc-rsync code defect. The test is marked XFAIL on the CI cell
through `tools/ci/upstream_testsuite_known_failures.conf` until a CI
validator workflow confirms that upstream rsync exhibits the same
failure in the same environment, at which point the in-tree IPv4
fallback documented here suffices and the XFAIL entry can be removed.

The deep-dive notes for the underlying investigation, the per-family
warning rationale, and the broader fix-sequencing context live in
`docs/design/uts-dd-fix-plan.md` under the `daemon-exit10` rows of
the root-cause matrix.
