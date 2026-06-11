# cli preflight rejection emission inventory (EDG-PREFLIGHT.1)

Baseline reference document. Inventories every cli call site that emits a
preflight-style error of the form "feature X not supported on this client",
"feature X not enabled at build time", or equivalent prose, classifies the
backing cfg/feature gate, cross-references the actual backend support in
`metadata`, `core`, `engine`, `compress`, `transfer`, and `fast_io`, and
calls out where the cli gate is more restrictive than the backend.

Scope: **cli crate only** (`crates/cli/`). Out-of-scope: daemon-mode access
control, server-side refuse-options, runtime fallback messages emitted
deeper in the stack.

## Scope: what counts as a preflight rejection

A **preflight rejection** is a cli-layer hard error that:

1. fires in the arg-parsing / config-build phase, before any I/O or wire
   protocol handshake;
2. rejects a user request because a Cargo feature, target_os cfg, or both
   excludes the requested capability from this build; and
3. matches one of the canonical strings:
   - `... not supported on this client`
   - `... not supported in this build`
   - `--<flag> requires the <feature> feature, which was disabled at build time`
   - or routes through `FEATURE_UNAVAILABLE_EXIT_CODE`.

The two `"... functionality is unavailable in this build (code N)"` strings
in `module_listing.rs:107` and `summary.rs:173` are **runtime stderr-write
fallback messages**, not preflight rejections. They fire only when
`write_message` itself fails to render an upstream `Message` to stderr.
They are listed in the inventory for completeness but classified as
out-of-scope.

The four `FEATURE_UNAVAILABLE_EXIT_CODE` sites in `options/protocol.rs`
are syntax-error mappings inside `--protocol=N` parsing, not feature-flag
rejections. They are similarly listed but classified as out-of-scope.

## Inventory

| Site (file:line) | Error string | Cfg gate today | Backend status | Verdict |
|---|---|---|---|---|
| `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:174-178` | `POSIX ACLs are not supported on this client` | `#[cfg(not(all(any(unix, windows), feature = "acl")))]` triggers when `--acls` is requested | `metadata` has `acl_exacl` (linux/macos/freebsd), `acl_stub` (ios/tvos/watchos), `acl_windows` (windows). All gated by `feature = "acl"`. CLI propagates `acl = ["core/acl"]` and `core` forwards to `metadata`. | **Correct.** Cfg gate aligns with backend coverage. Fires only when build lacks the `acl` Cargo feature, or target is an exotic non-unix/non-windows platform with no ACL impl. Locked in by `acls_accepted_on_{linux,macos,windows}_with_feature` tests + `acls_rejected_without_feature`. |
| `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:184-189` | `extended attributes are not supported on this client` | `#[cfg(not(all(any(unix, windows), feature = "xattr")))]` triggers when `--xattrs` is requested | `metadata` has `xattr_unix` (Linux/macOS via the `xattr` crate) and `xattr_windows` (FindFirstStreamW ADS impl, 20.9 KB module, wired by WPC-3 series). Both gated by `feature = "xattr"`. CLI default features = `["zstd", "lz4", "xattr"]`. CLI feature `xattr = ["core/xattr"]` and `core/xattr` enables `metadata` `xattr`. | **Correct.** Cfg gate was deliberately widened from `unix`-only to `any(unix, windows)` by the WPC-3.wire.1 series after `xattr_windows.rs` shipped. Locked in by `xattrs_accepted_on_{linux,macos,windows}_with_feature` tests. |
| `crates/cli/src/frontend/execution/drive/config.rs:140-141` | (no preflight emission; field declaration only) | `#[cfg(all(unix, feature = "xattr"))]` on the `xattrs: bool` field in `CoreConfig` | `metadata` supports xattr on **any(unix, windows)**, not just `unix`. The matching builder wiring at `config.rs:358` is also `cfg(all(unix, feature = "xattr"))`-gated, and the run-config consumer at `workflow/run.rs:828` mirrors that. | **Over-narrow (widen).** This is the cfg-propagation gap WPC-3.wire.2 was opened to fix. The preflight in `preflight.rs:184` accepts `--xattrs` on Windows + xattr feature, but the downstream `CoreConfig.xattrs` field is `cfg(all(unix, feature = "xattr"))`. Result: on Windows + xattr build, preflight passes but the value never reaches `CoreConfig` because the field doesn't exist; the request is silently dropped instead of executed. Track as **EDG-PREFLIGHT.1.W1** below. (No emission string to widen; the fix is to widen the field cfg from `unix` to `any(unix, windows)` and confirm `core` propagates the same shape.) |
| `crates/cli/src/frontend/execution/options/iconv.rs:67-74` | `--iconv requires the iconv feature, which was disabled at build time` | `#[cfg(not(feature = "iconv"))]` on `accept_parsed_setting` | `core` defines `iconv` feature; CLI declares `iconv = ["core/iconv"]`. The `iconv` feature is not in CLI default features. Backend support exists wherever `core/iconv` compiles (target-OS-agnostic, uses the `iconv-bin` / portable iconv crate). | **Correct.** Pure feature-flag gate, no platform restriction needed. Closes upstream issue #1915 (silent no-op when feature disabled). Note: error string uses different prose ("disabled at build time" vs "not supported on this client") - inventory captures both phrasings as the canonical preflight family. |
| `crates/cli/src/frontend/execution/options/protocol.rs:35,39,43,47` | `invalid protocol version 'X': protocol value must not be empty / must be an unsigned integer / cannot be negative / value exceeds 255; supported protocols are ...` | `FEATURE_UNAVAILABLE_EXIT_CODE` mapping for `--protocol` syntax errors | n/a - syntax-error path, not a feature-flag rejection | **Out of scope.** These are user-input syntax errors mapped to upstream exit code 1, not "feature is unavailable in this build". The exit-code constant is named `FEATURE_UNAVAILABLE_EXIT_CODE` to mirror upstream rsync's `RERR_SYNTAX` reuse, but the emission path is parse error, not preflight feature gating. Listed for completeness so future audits don't misclassify. |
| `crates/cli/src/frontend/execution/drive/module_listing.rs:107` | `rsync error: daemon functionality is unavailable in this build (code N)` | none (runtime fallback) | n/a - daemon support is unconditional in `cfg(unix)` builds via `crates/cli/Cargo.toml`'s `[target.'cfg(unix)'.dependencies] daemon = { path = "../daemon" }`. On Windows the daemon dep isn't pulled, and `module_listing` is reachable only when daemon support exists. | **Out of scope.** Runtime fallback printed when `write_message(error.message(), stderr)` itself fails (e.g. closed stderr). Not a preflight feature rejection. |
| `crates/cli/src/frontend/execution/drive/summary.rs:173` | `rsync error: client functionality is unavailable in this build (code 1)` | none (runtime fallback) | n/a | **Out of scope.** Same pattern as the module_listing site - fallback used only when `emit_message_with_fallback` can't write the actual upstream `Message`. Not a preflight feature rejection. |

## Findings

- **1 over-narrow gate** (config.rs:140). The field-cfg restriction is what
  WPC-3.wire.2 was opened to track. It is not an emission site itself, but
  it makes the *accepted* preflight result a silent no-op on Windows.
- **3 correct gates** in active use: ACLs preflight, xattrs preflight,
  iconv build-time rejection. The xattrs gate already incorporates the
  WPC-3.wire.1 widening so Windows + xattr builds pass preflight.
- **0 outdated emission strings.** No dead "feature X not supported" prose
  remains from a feature that has since been universally enabled.
- **2 runtime-fallback strings** misclassifiable as preflight; recorded
  explicitly so they are not re-investigated. The `*_unavailable in this
  build` prose is reserved for runtime, never preflight.
- **4 syntax-error sites** sharing the `FEATURE_UNAVAILABLE_EXIT_CODE`
  constant are documented and explicitly out of scope.

## Follow-up tasks

This audit is documentation-only. Implementation tasks (each a separate
PR) are filed as follows; this audit does not implement any of them.

- **EDG-PREFLIGHT.1.W1** - Widen `CoreConfig.xattrs` field cfg from
  `cfg(all(unix, feature = "xattr"))` to `cfg(all(any(unix, windows),
  feature = "xattr"))` at `crates/cli/src/frontend/execution/drive/config.rs:140`.
  Update the matching `.xattrs()` builder call at line 358 and the run-config
  consumer at `crates/cli/src/frontend/execution/drive/workflow/run.rs:828`.
  Audit `core::config::CoreConfigBuilder::xattrs` and downstream consumers
  for the same `unix`-only gate, widening each site that mirrors this one.
  Add regression test asserting `CoreConfig::xattrs` actually carries the
  `--xattrs=true` value through to the engine when built on Windows with
  the `xattr` feature. This closes the silent-no-op gap left by the
  successful preflight widening.
- **EDG-PREFLIGHT.1.W2** - Convention doc: write a short coding-guide entry
  for "preflight cfg gates must match backend cfg gates exactly; mismatches
  produce silent no-ops". Reference the WPC-3.wire sequence as the
  canonical example of a 3-step fix (cli preflight gate, cli config field
  gate, downstream config propagation). EDG-PREFLIGHT.4 is the parent
  task; this audit feeds its acceptance criterion.

## Verification

The inventory was produced by greping `crates/cli/` for the canonical
emission-string families:

```sh
grep -rn "not supported on this client" crates/cli/
grep -rn "not supported in this build" crates/cli/
grep -rn "feature.*not.*enabled" crates/cli/
grep -rn "not-supported-on-this-client" crates/cli/
grep -rn "FEATURE_UNAVAILABLE" crates/cli/
grep -rn "compiled without\|disabled at build\|was disabled\|requires.*feature" crates/cli/src/
grep -rn "rsync error.*unavailable\|rsync error.*not supported" crates/cli/src/
```

Each hit was inspected, classified, and either entered into the table
above or rejected with a documented reason. Backend support was
cross-checked against:

- `crates/metadata/src/lib.rs` (lines 70-186) for ACL and xattr platform
  matrix;
- `crates/metadata/src/xattr_windows.rs` and `crates/metadata/src/acl_windows/`
  for Windows backend presence;
- `crates/cli/Cargo.toml` for the cli feature surface and default features;
- `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:201-345`
  test module for the locked-in expected behaviour matrix.
