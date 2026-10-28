//! Selection between the in-process local engine and the `--server` child.

use logging::{DebugFlag, debug_gte};

use super::super::config::ClientConfig;

/// Reports whether a local copy runs as upstream's sender parent plus a
/// `--server` child, instead of the in-process engine.
///
/// Upstream always forks for a local copy (main.c:666-667 `local_child()`),
/// and its `-vvv` role traces come from that split. The in-process engine
/// has no sender, generator and receiver to emit them, so the split runs
/// only when one of those traces is requested; every other local copy keeps
/// the engine. Listings and batch runs stay on the engine, which owns their
/// output.
pub(super) fn local_copy_uses_server_child(config: &ClientConfig) -> bool {
    role_trace_requested() && !config.list_only() && config.batch_config().is_none()
}

/// The per-role debug traces that `-vvv` enables (options.c:248) and that
/// only the sender, generator and receiver roles emit.
pub(crate) fn role_trace_requested() -> bool {
    debug_gte(DebugFlag::Genr, 1)
        || debug_gte(DebugFlag::Recv, 1)
        || debug_gte(DebugFlag::Send, 1)
        || debug_gte(DebugFlag::Exit, 1)
        || debug_gte(DebugFlag::Flist, 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use logging::{VerbosityConfig, init};

    fn with_debug(spec: &str) {
        let mut verbosity = VerbosityConfig::default();
        verbosity.apply_debug_flag(spec).expect("valid debug flag");
        init(verbosity);
    }

    fn local_config() -> ClientConfig {
        ClientConfig::builder().transfer_args(["src/", "dst/"]).build()
    }

    #[test]
    fn a_role_trace_selects_the_server_child() {
        for spec in ["genr", "recv", "send", "exit", "flist2"] {
            with_debug(spec);
            assert!(
                local_copy_uses_server_child(&local_config()),
                "--debug={spec} is emitted only by the split roles"
            );
        }
    }

    #[test]
    fn an_untraced_copy_keeps_the_in_process_engine() {
        // FLIST1 is printed by the engine itself; only FLIST2 needs the roles.
        for spec in ["none", "flist1", "deltasum2", "filter2"] {
            with_debug(spec);
            assert!(
                !local_copy_uses_server_child(&local_config()),
                "--debug={spec} must not leave the fast engine"
            );
        }
    }

    #[test]
    fn a_listing_keeps_the_engine_even_when_traced() {
        with_debug("genr");
        let config = ClientConfig::builder()
            .transfer_args(["src/"])
            .list_only(true)
            .build();
        assert!(!local_copy_uses_server_child(&config));
    }
}
