#![allow(clippy::module_name_repetitions)]

//! JSON serialisation helpers for workspace branding metadata.
//!
//! The [`manifest_json`] and [`manifest_json_pretty`] helpers expose cached
//! JSON renderings of the [`BrandManifest`][super::manifest::BrandManifest].

use std::sync::OnceLock;

use super::manifest::manifest;

fn render_manifest_json(pretty: bool) -> String {
    let manifest = manifest();

    if pretty {
        serde_json::to_string_pretty(manifest)
            .expect("failed to serialise branding manifest as pretty JSON")
    } else {
        serde_json::to_string(manifest).expect("failed to serialise branding manifest as JSON")
    }
}

/// Returns the cached JSON representation of the workspace branding manifest.
///
/// The returned string is cached for the lifetime of the process and reflects
/// the data exposed by [`manifest`].
///
/// ```
/// let json = branding::branding::manifest_json();
/// assert!(json.contains("\"client_program_name\":\"oc-rsync\""));
/// ```
#[must_use]
pub fn manifest_json() -> &'static str {
    static JSON: OnceLock<String> = OnceLock::new();
    JSON.get_or_init(|| render_manifest_json(false)).as_str()
}

/// Returns the cached pretty-printed JSON representation of the branding
/// manifest.
///
/// Mirrors [`manifest_json`] but emits indented human-readable output.
///
/// ```
/// let json = branding::branding::manifest_json_pretty();
/// assert!(json.lines().any(|line| line.contains("\"daemon_program_name\"")));
/// ```
#[must_use]
pub fn manifest_json_pretty() -> &'static str {
    static JSON_PRETTY: OnceLock<String> = OnceLock::new();
    JSON_PRETTY
        .get_or_init(|| render_manifest_json(true))
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_json_matches_direct_serialisation() {
        let expected = serde_json::to_string(manifest()).expect("failed to serialise manifest");
        assert_eq!(manifest_json(), expected);
    }

    #[test]
    fn manifest_json_pretty_matches_direct_serialisation() {
        let expected =
            serde_json::to_string_pretty(manifest()).expect("failed to serialise manifest");
        assert_eq!(manifest_json_pretty(), expected);
    }
}
