//! Cargo feature-propagation regression tests for `crates/core`.
//!
//! PR #5564 fixed a propagation gap where the `core` crate's `xattr` feature
//! pulled in `transfer/xattr` but NOT `metadata/xattr`. The `metadata` crate
//! owns the Windows FindFirstStreamW ADS backend, so without the propagation
//! the backend was dead code on Windows even when `--features xattr` was set.
//!
//! These tests parse `crates/core/Cargo.toml` statically (no `cargo metadata`
//! subprocess) and assert that every CLI-facing platform feature reaches the
//! `metadata` sub-crate when it has an implementation there. They lock in the
//! propagation invariant so a future edit to the feature list that drops the
//! `metadata/...` arm fails CI loudly instead of silently disabling shipped
//! functionality on Windows or any other platform that ships a backend.
//!
//! Convention: see `docs/contributing/ONBOARDING.md` ("Platform-feature gates")
//! for the rule this test enforces.

use std::path::PathBuf;

/// Returns the contents of `crates/core/Cargo.toml`, located relative to this
/// crate's manifest dir so the test works from any cargo working directory.
fn read_core_manifest() -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let core_manifest = manifest_dir
        .parent()
        .expect("crates/cli has a parent (crates/)")
        .join("core")
        .join("Cargo.toml");
    std::fs::read_to_string(&core_manifest)
        .unwrap_or_else(|err| panic!("read {}: {err}", core_manifest.display()))
}

/// Returns the propagation arms for the named feature in `[features]`, e.g.
/// `["metadata/xattr", "transfer/xattr"]` for `xattr`.
fn feature_arms(manifest: &str, feature: &str) -> Vec<String> {
    let value: toml::Table = manifest.parse().expect("parse crates/core/Cargo.toml");
    let features = value
        .get("features")
        .and_then(toml::Value::as_table)
        .expect("[features] table in crates/core/Cargo.toml");
    let arms = features
        .get(feature)
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("feature `{feature}` is not an array"));
    arms.iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("feature `{feature}` arm is not a string: {v:?}"))
                .to_string()
        })
        .collect()
}

#[test]
fn core_xattr_feature_propagates_to_metadata_subcrate() {
    let manifest = read_core_manifest();
    let arms = feature_arms(&manifest, "xattr");
    assert!(
        arms.iter().any(|a| a == "metadata/xattr"),
        "`xattr` feature in crates/core/Cargo.toml must include `metadata/xattr` so the \
         metadata crate's xattr backend (FindFirstStreamW on Windows, listxattr/getxattr on \
         Linux/macOS) is actually compiled in. Without it, --xattrs becomes a no-op even \
         when the CLI advertises the feature. See PR #5564 (WPC-3 reality fix). Got: {arms:?}",
    );
    assert!(
        arms.iter().any(|a| a == "transfer/xattr"),
        "`xattr` feature must continue to propagate to `transfer/xattr` so the wire path \
         carries xattr metadata. Got: {arms:?}",
    );
}

#[test]
fn core_acl_feature_propagates_to_metadata_subcrate() {
    let manifest = read_core_manifest();
    let arms = feature_arms(&manifest, "acl");
    assert!(
        arms.iter().any(|a| a == "metadata/acl"),
        "`acl` feature in crates/core/Cargo.toml must include `metadata/acl` so the metadata \
         crate's ACL backend (exacl on Unix, DACL on Windows) is compiled in. Got: {arms:?}",
    );
    assert!(
        arms.iter().any(|a| a == "engine/acl"),
        "`acl` feature must propagate to `engine/acl`. Got: {arms:?}",
    );
    assert!(
        arms.iter().any(|a| a == "transfer/acl"),
        "`acl` feature must propagate to `transfer/acl`. Got: {arms:?}",
    );
}

/// Symmetry guard: if a future edit ever drops `metadata/<feat>` from the
/// `acl` arm, the test above catches it. This second test additionally
/// enforces that the two platform-metadata features stay structurally
/// symmetric - both reach the `metadata` crate. The motivating defect (PR
/// #5564) was exactly that `xattr` and `acl` had asymmetric propagation.
#[test]
fn core_platform_metadata_features_stay_symmetric() {
    let manifest = read_core_manifest();
    let xattr_arms = feature_arms(&manifest, "xattr");
    let acl_arms = feature_arms(&manifest, "acl");

    let xattr_reaches_metadata = xattr_arms.iter().any(|a| a == "metadata/xattr");
    let acl_reaches_metadata = acl_arms.iter().any(|a| a == "metadata/acl");

    assert_eq!(
        xattr_reaches_metadata, acl_reaches_metadata,
        "xattr and acl features must both propagate to the metadata sub-crate (or neither). \
         Asymmetric propagation is the WPC-3 defect class. xattr arms: {xattr_arms:?}, \
         acl arms: {acl_arms:?}",
    );
}
