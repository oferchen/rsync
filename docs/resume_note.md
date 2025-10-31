# Resume Notes

## Status
- Workspace implements the canonical **rsync 3.4.1-rust** and **rsyncd 3.4.1-rust** binaries with a deterministic local copy engine, message formatting, and protocol negotiation scaffolding that already mirrors upstream interfaces for local execution. Compatibility wrappers (**oc-rsync**, **oc-rsyncd**) expose the same execution paths for environments that still reference the legacy branding.
- Remote transfers still delegate to the system `rsync` binary while the native transport and delta pipeline are being integrated; remaining observable gaps are tracked in `docs/differences.md` and scoped by `docs/production_scope_p1.md`.
- Documentation, branding, and CI guardrails (lint, coverage, packaging, cross-compilation) are established and enforced via the consolidated `ci.yml` workflow.

## Suggested Next Steps
1. Continue burning down the parity gaps by wiring the engine and protocol crates together, replacing the remote fallback while maintaining byte-for-byte compatibility with upstream rsync 3.4.1.
2. Expand the parity harness (golden transcripts, exit-code oracle, interop matrix) so upstream comparisons run automatically in CI.
3. Keep the documentation set (`docs/differences.md`, `docs/gaps.md`, `docs/feature_matrix.md`) synchronized with observed behaviour after each feature landing.
4. Maintain the LoC hygiene guard by refactoring any module that approaches the 600-line ceiling into focused submodules.

## Recent Verification
- `cargo test` (workspace default profile) to validate unit tests, integration tests, and doctests across all crates.
