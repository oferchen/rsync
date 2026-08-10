//! JSON serialisation helpers for [`crate::workspace`] metadata.
//!
//! Publishes cached JSON renderings derived from the canonical
//! [`Metadata`](super::Metadata) snapshot so packaging automation and
//! documentation tooling can consume the same data in a structured format.

use std::sync::OnceLock;

use super::{Metadata, metadata};

fn render_metadata_json(pretty: bool) -> String {
    let snapshot: Metadata = metadata();

    if pretty {
        serde_json::to_string_pretty(&snapshot)
            .expect("failed to serialise workspace metadata as pretty JSON")
    } else {
        serde_json::to_string(&snapshot).expect("failed to serialise workspace metadata as JSON")
    }
}

/// Returns the cached JSON representation of the workspace metadata snapshot.
///
/// The returned string mirrors [`metadata()`](super::metadata) and is cached
/// for the lifetime of the process.
///
/// ```
/// let json = branding::workspace::metadata_json();
/// assert!(json.contains("\"brand\":\"oc\""));
/// assert!(json.contains("\"rust_version\":"));
/// ```
#[must_use]
pub fn metadata_json() -> &'static str {
    static JSON: OnceLock<String> = OnceLock::new();
    JSON.get_or_init(|| render_metadata_json(false)).as_str()
}

/// Returns the cached pretty-printed JSON representation of the workspace
/// metadata snapshot.
///
/// Mirrors [`metadata_json`] but renders indented human-friendly output.
///
/// ```
/// let json = branding::workspace::metadata_json_pretty();
/// assert!(json.lines().any(|line| line.contains("\"daemon_program_name\"")));
/// ```
#[must_use]
pub fn metadata_json_pretty() -> &'static str {
    static JSON_PRETTY: OnceLock<String> = OnceLock::new();
    JSON_PRETTY
        .get_or_init(|| render_metadata_json(true))
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_json_matches_direct_serialisation() {
        let expected = serde_json::to_string(&metadata()).expect("failed to serialise metadata");
        assert_eq!(metadata_json(), expected);
    }

    #[test]
    fn metadata_json_pretty_matches_direct_serialisation() {
        let expected =
            serde_json::to_string_pretty(&metadata()).expect("failed to serialise metadata");
        assert_eq!(metadata_json_pretty(), expected);
    }
}
