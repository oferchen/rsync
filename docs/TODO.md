
## compression encapsulation using identical algorthms as upstream, validate hashing use the same algo as upstream. 
## ci interop exit code is omitted using || true

## UPSTREAM-CONSISTENT DAEMON MODEL

**Upstream model:** one binary (`rsync`) enters daemon mode with `--daemon`; supports standalone listen or inetd/systemd socket activation; configured via `rsyncd.conf(5)`. We must mirror this exactly. :contentReference[oaicite:10]{index=10}

**Therefore:**
- **Main binary:** `oc-rsync` handles client **and** daemon (`--daemon`). Detect inetd/systemd by socket on stdin and avoid double-fork, as upstream. :contentReference[oaicite:11]{index=11}
- **Wrapper:** `oc-rsyncd` is **only** a thin wrapper/symlink executing `oc-rsync --daemon "$@"`. No logic divergence.
- **Config:** parse upstream `rsyncd.conf` semantics (modules, `auth users`, `secrets`, `hosts allow/deny`, `read only`, `uid/gid`, `chroot`, `timeout`, `refuse options`), defaulting to `/etc/oc-rsyncd/oc-rsyncd.conf`. :contentReference[oaicite:12]{index=12}
- **Docs:** `oc-rsync --daemon --help` mirrors upstream’s daemon-options help layout. :contentReference[oaicite:13]{index=13}

---

## 🧭 NO-ARGS USAGE PARITY (FIX CURRENT DIVERGENCE)

**Observed upstream behavior:** Running `rsync` with **no args** prints:
1) header: name/version/protocol & copyright; 2) capabilities; 3) optimizations; 4) checksum/compress lists; 5) daemon auth list; 6) multi-form **Usage** block; 7) final error `syntax or usage error (code 1)` line. Our `oc-rsync` must match this order, text, and code. :contentReference[oaicite:14]{index=14}

**Requirements:**
- Replace the short custom “missing source operands” banner with the **full upstream-shaped** preamble+usage, then (optionally) append our specific hint; exit with **code 1**.
- Snapshot test compares `oc-rsync` (no args) output to an upstream **golden** (allowing only binary name & branding/path differences). :contentReference[oaicite:15]{index=15}

