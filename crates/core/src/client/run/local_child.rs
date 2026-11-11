//! Selection between the sender-plus-receiver split and the local engine.

use super::super::config::ClientConfig;

/// Reports whether a local copy runs as upstream's sender plus a server
/// receiver, instead of the in-process engine.
///
/// Upstream runs every local copy that way (main.c:666-667 `local_child()`).
/// A listing has no receiver (main.c:721 `get_local_name()` returns NULL
/// under `list_only`) and a batch run is replayed or recorded by the engine,
/// which owns both outputs, so only those two stay on the engine.
#[must_use]
pub fn local_copy_uses_server_child(config: &ClientConfig) -> bool {
    !config.list_only() && config.batch_config().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_config() -> ClientConfig {
        ClientConfig::builder()
            .transfer_args(["src/", "dst/"])
            .build()
    }

    // WHY: upstream splits every local copy, at every verbosity; a copy that
    // stayed on the engine would print none of the per-role lines.
    #[test]
    fn every_local_copy_runs_the_split_roles() {
        assert!(local_copy_uses_server_child(&local_config()));
    }

    // WHY: a listing has no receiver to split off.
    #[test]
    fn a_listing_keeps_the_engine() {
        let config = ClientConfig::builder()
            .transfer_args(["src/"])
            .list_only(true)
            .build();
        assert!(!local_copy_uses_server_child(&config));
    }
}
