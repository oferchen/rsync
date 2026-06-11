# EDG-PREFLIGHT.1 - CLI preflight feature-gate inventory

Audit phase for issue #3900. Enumerates every client-side preflight
emission point that rejects a requested CLI option because the
corresponding feature was disabled at build time or because the target
platform does not support it. The output of this audit feeds the
follow-up tasks EDG-PREFLIGHT.2/.3/.4 and informs the
WPC-3.wire.1 fix for Windows xattr.

## Scope

A "preflight check" for this audit is a CLI-side gate that:

1. Returns a hard error (exit code 1) before any transfer begins.
2. Is guarded by a `cfg(...)` expression on a feature flag or platform.
3. Tells the user that the requested feature is not available on this
   build or platform (rather than silently no-opping).

Server-side rejections, runtime fallbacks, and wire-protocol
negotiation failures are out of scope here; those are tracked
separately (see Cross-references).

## Inventory

| Flag | Source file:line | cfg expression | Feature flag | Emission text | Crate dependency target |
|---|---|---|---|---|---|
| `--acls` / `-A` | `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:165-171` | `cfg(not(all(any(unix, windows), feature = "acl")))` | `acl` (defaults to off) | `"POSIX ACLs are not supported on this client"` | `crates/metadata` (acl module) via `crates/core` re-export |
| `--xattrs` / `-X` | `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:176-182` | `cfg(not(all(unix, feature = "xattr")))` | `xattr` (defaults on) | `"extended attributes are not supported on this client"` | `crates/metadata` (xattr module) via `crates/core` re-export |
| `--iconv=LOCAL,REMOTE` | `crates/cli/src/frontend/execution/options/iconv.rs:67-74` | `cfg(not(feature = "iconv"))` | `iconv` (defaults off) | `"--iconv requires the iconv feature, which was disabled at build time"` | `crates/core` (iconv module); upstream iconv lib |
| `--usermap=SPEC` | `crates/cli/src/frontend/execution/drive/metadata/mapping.rs:71-86` | `cfg(windows)` (no feature flag) | n/a (platform gate) | `"--usermap is not supported on Windows builds of oc-rsync"` | `crates/metadata` (POSIX id lookup); `uzers` Unix-only dep |
| `--groupmap=SPEC` | `crates/cli/src/frontend/execution/drive/metadata/mapping.rs:91-106` | `cfg(windows)` (no feature flag) | n/a (platform gate) | `"--groupmap is not supported on Windows builds of oc-rsync"` | `crates/metadata` (POSIX id lookup); `uzers` Unix-only dep |
| `--daemon` | `crates/cli/src/frontend/server/daemon.rs:201-220` | `cfg(windows)` (no feature flag) | n/a (platform gate) | `"daemon mode is not supported on this platform; run the oc-rsync daemon on a Unix-like system"` | `crates/daemon` (Unix-only `[target.'cfg(unix)'.dependencies]`) |

Total preflight checks found: **6**.

## Notes on cfg expression vs actual platform availability

### Match analysis

- **`--acls`** - `cfg(not(all(any(unix, windows), feature = "acl")))` correctly
  fires only when the platform is non-Unix and non-Windows *or* when the
  `acl` feature was compiled out. Both POSIX (`exacl`) and Windows
  (`windows-rs`) ACL backends are exercised when the feature is on, so
  the cfg expression matches the dependency target. No gap.

- **`--xattrs`** - `cfg(not(all(unix, feature = "xattr")))` rejects on
  Windows even when the `xattr` feature is enabled. Windows NTFS ADS
  (Alternate Data Streams) is the local equivalent of POSIX extended
  attributes, but the preflight cfg test fails on Windows because it
  requires `unix`. This is the **WPC-3.wire.1** gap: the cfg gate is
  too narrow for the actual platform availability set the
  user-facing semantics could provide.

- **`--iconv`** - `cfg(not(feature = "iconv"))` is a pure feature-flag
  gate with no platform qualifier. The dependency target (`iconv` C
  library) is portable; the gate correctly reflects build-time
  availability. No gap.

- **`--usermap` / `--groupmap`** - `cfg(windows)` rejects unconditionally
  on Windows because `uzers` and the POSIX name-lookup APIs are
  Unix-only. A future Windows-native equivalent would need to use SID
  lookups (`LookupAccountNameW`) and the cfg gate would need to relax;
  for now the gate accurately reflects backend availability. No gap.

- **`--daemon`** - `cfg(windows)` rejects on Windows because the daemon
  crate is `[target.'cfg(unix)'.dependencies]`-only in
  `crates/cli/Cargo.toml`. Cfg gate matches dependency target. No gap.

### Inconsistencies surfaced

1. **Feature-flag wording divergence**: only `--iconv` mentions "was
   disabled at build time" in its message. The ACL and xattr messages
   say "not supported on this client" without distinguishing
   build-time-disabled from platform-unsupported. The `--usermap` /
   `--groupmap` / `--daemon` messages name the platform explicitly. A
   convention pass (EDG-PREFLIGHT.4) should pick one phrasing per
   axis.

2. **`Brand` in emission text**: `--usermap` and `--groupmap` hard-code
   `"oc-rsync"`; the others do not name the brand. This is fine for
   build-time-disabled features but misleading when the upstream
   `rsync` binary name is in use via the brand dispatch (see
   `crates/branding`). EDG-PREFLIGHT.4 should normalize this.

3. **Location of the gate**: only the ACL and xattr gates live in the
   designated home `preflight.rs`. The `--iconv` rejection lives in
   `options/iconv.rs` (config-build time) and the
   `--usermap` / `--groupmap` / `--daemon` rejections live in their
   own modules. EDG-PREFLIGHT.4 should either centralize all preflight
   checks under `preflight.rs` or document the per-module convention
   explicitly.

4. **Test coverage**: `crates/cli/src/frontend/tests/acls.rs` and
   `xattrs.rs` cover only the feature-off path. There is no positive
   preflight test for `--iconv` rejection wiring, no Windows-platform
   test for `--usermap` / `--groupmap`, and no test that
   `--daemon` on Windows emits the expected message. EDG-PREFLIGHT.2
   covers these gaps.

## Cross-references

- **EDG-PREFLIGHT.2 (#3901)** - edge tests confirming each
  platform-feature flag passes preflight when feature is enabled.
  This audit names the 6 preflight gates and the 4 messages whose
  positive-case coverage is currently missing; .2 turns each into a
  test in `crates/cli/src/frontend/tests/`.

- **EDG-PREFLIGHT.3 (#3902)** - cross-check CLI feature flags
  propagate to dependent crates. Inventory column "Crate dependency
  target" enumerates the per-gate dependency edge that .3 must walk:
  `cli/acl -> core/acl -> metadata::acl`; `cli/xattr -> core/xattr
  -> metadata::xattr`; `cli/iconv -> core/iconv`. .3 also verifies
  that disabling a feature in `cli` does not leave an orphaned
  feature on in a leaf crate.

- **EDG-PREFLIGHT.4 (#3903)** - document preflight cfg-gate
  convention. The "Inconsistencies surfaced" section above lists the
  four conventions that need to be settled: message wording, brand
  inclusion, gate location, and the platform-vs-feature-flag axis.

- **WPC-3.wire.1.b / .c / .d (#3815-#3817)** - fix preflight cfg
  gate to allow Windows + xattr. This audit confirms the gap is in
  `preflight.rs:176` where `cfg(not(all(unix, feature = "xattr")))`
  excludes the `windows + xattr` cell that NTFS ADS can satisfy.
  WPC-3.wire.1.b/.c/.d should relax the cfg to
  `cfg(not(all(any(unix, windows), feature = "xattr")))` and wire the
  Windows ADS backend through the same metadata code path the POSIX
  backend uses.
