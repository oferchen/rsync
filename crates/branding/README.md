# branding

The `branding` crate centralises branding, version, and workspace
metadata for the Rust-based rsync reimplementation. It exposes the canonical
program names, daemon configuration paths, source URL, and build revision as
compile-time constants derived from the workspace `Cargo.toml`. Higher-level
crates import this crate to render CLI banners, construct diagnostics, and
validate packaging artefacts without duplicating string literals or parsing the
workspace manifest at runtime.

The crate is intentionally lightweight and free of I/O at runtime: all metadata
is compiled into the binary by the build script so callers can depend on
zero-cost `const` accessors. This keeps the overall workspace maintainable
while ensuring branded values remain consistent across binaries, documentation,
and packaging logic.
