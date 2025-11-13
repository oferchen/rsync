# Exit Codes, Diagnostics, and Version Branding

This note captures how oc-rsync mirrors upstream rsync 3.4.1’s user-facing diagnostics, error codes, and `--version` output.

## Exit code catalogue

* `crates/core::message::strings` embeds the upstream `rerr_names` table so every documented exit code is backed by the canonical severity and diagnostic text. The table covers syntax errors, protocol failures, timeout conditions, remote shell errors, and the 124–127 range that scripts frequently check.【F:crates/core/src/message/strings.rs†L90-L133】
* Helpers convert a selected template into a `Message`, ensuring the textual descriptions stay aligned with upstream wording when surfaced to users or logs.【F:crates/core/src/message/strings.rs†L135-L197】
* The CLI clamps all exit codes to the 0–255 range before returning from `main`, matching the behaviour of the C implementation on Unix hosts.【F:crates/cli/src/frontend/mod.rs†L316-L322】

### Frequently referenced codes

| Code | Meaning | Notes |
| ---- | ------- | ----- |
| 0 | Success | No differences detected or all requested copies completed. |
| 1 | Syntax or usage error | Raised when clap reports argument conflicts (e.g. multiple delete-timing flags).【F:crates/core/src/message/strings.rs†L90-L106】【F:crates/cli/src/frontend/arguments/parser.rs†L118-L147】 |
| 23 | Partial transfer | Propagated when some files could not be copied even though the run continued.【F:crates/core/src/message/strings.rs†L110-L118】 |
| 24 | Vanished source | Emitted when files disappear mid-transfer; surfaced as a warning in the native engine tests.【F:crates/core/src/message/strings.rs†L118-L124】 |
| 124–127 | Remote shell failures | Set when the fallback runner cannot launch or communicate with the remote shell binary.【F:crates/core/src/message/strings.rs†L125-L133】【F:crates/core/src/client/fallback/runner/mod.rs†L1-L73】 |

## Message trailers and roles

* Every diagnostic attaches the Rust source suffix via `VERSION_SUFFIX`, which resolves to the shared workspace version string. This mirrors the C implementation’s habit of appending the module/role that emitted the message.【F:crates/core/src/message/mod.rs†L22-L24】
* Role annotations (`Client`, `Generator`, `Receiver`, etc.) flow through the high-level helpers so parity scripts can continue parsing the trailers they expect from upstream rsync.【F:crates/cli/src/frontend/mod.rs†L300-L318】【F:crates/core/src/message/strings.rs†L135-L197】

## Version banners

* The branding module publishes `RUST_VERSION = "3.4.1-rust"` alongside the upstream base version, copyright span, and daemon names. All CLI entry-points consume this metadata when rendering `--version` so operators see the same capability lists as upstream with Rust-specific suffixes.【F:crates/core/src/version/mod.rs†L1-L54】
* `VersionInfoReport` composes the standard `rsync --version` output, including checksum lists and compression support, using the same ordering as rsync 3.4.1 while substituting the oc-rsync source URL and build revision.【F:crates/core/src/version/report/renderer.rs†L13-L192】
* The help banner references the branded daemon name, reinforcing the workspace guidance that packaging should install a single `oc-rsync` binary plus optional compatibility symlinks.【F:crates/cli/src/frontend/help.rs†L19-L44】

## Outstanding work

* Audit the remaining call-sites that map low-level errors into exit codes to confirm their severities match the upstream `rerr_names` table.
* Extend the integration tests to snapshot `--version` output across different feature flags so changes in compiled compressor/checksum support stay visible during reviews.
