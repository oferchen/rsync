# ICV-2: iconv Feature Flag Audit

Date: 2026-06-03

## Feature Flag Status

The `iconv` feature propagates through the workspace dependency tree:

| Crate | Feature definition | Backend |
|-------|--------------------|---------|
| `bin` (workspace root) | `iconv = ["cli/iconv", "core/iconv"]` | default-on |
| `cli` | `iconv = ["core/iconv"]` | forwards to core |
| `core` | `iconv = ["protocol/iconv", "transfer/iconv", "engine/iconv"]` | default-on |
| `daemon` | `iconv = ["core/iconv", "protocol/iconv"]` | opt-in (not default) |
| `engine` | `iconv = ["protocol/iconv"]` | forwards to protocol |
| `transfer` | `iconv = ["protocol/iconv"]` | forwards to protocol |
| `protocol` | `iconv = ["encoding_rs"]` | leaf - gates `encoding_rs` |

When disabled, `protocol::iconv::FilenameConverter` compiles as a zero-field
struct with identity-only stubs. Non-UTF-8 encoding names return
`ConversionError` from `FilenameConverter::new()`.

## CLI Parsing Status

`--iconv=LOCAL,REMOTE` and `--no-iconv` are fully parsed:

- `crates/cli/src/frontend/arguments/parser/mod.rs` - Clap argument definition.
- `crates/cli/src/frontend/execution/options/iconv.rs` - `resolve_iconv_setting()`
  maps the raw `OsStr` to `IconvSetting`.
- When the `iconv` feature is compiled out, `accept_parsed_setting()` returns a
  hard error: `"--iconv requires the iconv feature, which was disabled at build
  time"` (exit code 1). This prevents silent no-op (#1915).
- `--no-iconv` and absence of `--iconv` are accepted regardless of feature gate.

## CI Coverage Status

The `iconv` feature is exercised in CI through multiple paths:

| CI Job | Feature coverage | Platform |
|--------|-----------------|----------|
| nextest (stable) | `--all-features` (includes iconv) | Linux |
| clippy | `--all-features` | Linux |
| Windows | `--all-features` on core/engine/cli | Windows |
| macOS | `--all-features` on core/engine/cli/metadata/apple-fs | macOS |
| Linux musl | `--no-default-features --features "zstd,lz4,xattr,iconv,..."` | Linux musl |
| Interop | `test_iconv`, `test_iconv_upstream_interop`, `test_iconv_local_ssh_interop` | Linux |

The interop harness (`tools/ci/run_interop.sh`) includes three dedicated iconv
test functions:

1. `test_iconv` - identity transfer (UTF-8,UTF-8) and cross-charset
   (UTF-8,ISO-8859-1) local-mode.
2. `test_iconv_upstream_interop` - daemon-mode round-trip vs upstream rsync
   3.4.1+, both directions.
3. `test_iconv_local_ssh_interop` - SSH/local-mode interop with fake-rsh,
   upstream as receiver and sender.

No CI job tests with the `iconv` feature *disabled*. The `#[cfg(not(feature =
"iconv"))]` test paths (e.g., `test_stub_only_supports_utf8`,
`resolve_iconv_setting_rejects_explicit_when_feature_off`) are never exercised
in CI because all jobs enable iconv.

## Test Coverage Status

Test coverage is thorough across crates when the feature is enabled:

| Location | Tests | Notes |
|----------|-------|-------|
| `protocol::iconv::mod` (inline) | 20 tests | Converter API, identity, round-trip, aliases, encoding errors. 12 gated on `#[cfg(feature = "iconv")]`, 1 on `#[cfg(not(feature = "iconv"))]`. |
| `protocol/tests/iconv_golden_bytes.rs` | 16 tests | Wire-level golden byte tests: sender, receiver, round-trip, suffix_len. All `#[cfg(feature = "iconv")]`, receiver tests also `#[cfg(unix)]`. |
| `cli::frontend::tests::iconv` | 4 tests | `resolve_iconv_setting` with explicit, disable, empty, feature-off rejection. |
| `cli::frontend::tests::parse_args_recognises_iconv` | 2 tests | Argument parsing for `--iconv` and `--no-iconv`. |
| `cli::frontend::execution::options::iconv` (inline) | 9 tests | Setting resolution, feature-off rejection, `--no-iconv` acceptance. |
| `core::client::config::iconv` (inline) | 19 tests | `IconvSetting::parse`, `resolve_converter`, `resolve_local_copy_converter`, debug emissions. |
| `engine/tests/iconv_local_copy.rs` | 4+ tests | Local-copy disk transcoding. Linux-only, iconv-gated. |
| `transfer::receiver::tests::file_list::iconv_wire_order` | 3 tests | Receiver sort-order preservation under iconv. |
| `protocol/tests/protocol_feature_gates.rs` | 1 test | `CF_SYMLINK_ICONV` compatibility flag. |

## Gaps Found

1. **No CI coverage for iconv-disabled builds.** All CI jobs enable
   `--all-features` or explicitly include `iconv`. The `#[cfg(not(feature =
   "iconv"))]` code paths - including the graceful rejection of `--iconv` when
   the feature is off - are tested only by `cfg`-gated unit tests that are
   compiled but never run in CI. A CI job with `--no-default-features --features
   "zstd,lz4,xattr"` (omitting iconv) would close this gap.

2. **Daemon crate does not default-enable iconv.** The `daemon` crate defines
   `iconv = ["core/iconv", "protocol/iconv"]` but does not include it in its
   default features. The `charset = ...` config directive in `oc-rsyncd.conf`
   depends on the binary-level default features propagating iconv to daemon via
   `core`. If someone builds daemon as a standalone crate without the workspace
   default, `charset` silently has no effect. Low risk - standalone daemon builds
   are not a supported workflow.

3. **`converter_from_locale()` always returns identity.** Both the enabled and
   disabled paths return `FilenameConverter::identity()` (UTF-8). On systems
   where the locale is genuinely non-UTF-8 (e.g., `LC_CTYPE=ja_JP.EUC-JP`),
   `--iconv=.` would not actually transcode. The code acknowledges this with a
   comment. This matches upstream rsync on modern systems but diverges on legacy
   locales.

4. **No interop test for non-ASCII filenames over SSH with upstream.** The SSH
   interop test (`test_iconv_local_ssh_interop`) uses `cafe\xcc\x81.txt` (a
   combining accent) as its only non-ASCII fixture. There is no coverage for
   multi-byte encodings (CJK via EUC-JP/Shift_JIS) or filenames with characters
   unmappable in the target charset.

5. **Engine integration tests are Linux-only.** The `iconv_local_copy.rs`
   integration tests are gated on `#[cfg(all(target_os = "linux", feature =
   "iconv"))]` because macOS APFS and Windows NTFS reject raw non-UTF-8 byte
   sequences. This is a filesystem limitation, not a code gap, but it means the
   local-copy iconv path has zero integration coverage on macOS and Windows.
