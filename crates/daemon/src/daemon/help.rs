use core::branding::{Brand, manifest, source_line};

/// Renders the deterministic daemon help text for the supplied branding profile.
pub(crate) fn help_text(brand: Brand) -> String {
    let manifest = manifest();
    let program = brand.daemon_program_name();
    let default_config = brand.daemon_config_path_str();

    format!(
        concat!(
            "{program} {version}\n",
            "{source_line}\n",
            "\n",
            "Usage: {program} [--help] [--version] [ARGS...]\n",
            "\n",
            "Daemon mode is under active development. This build recognises:\n",
            "  --help        Show this help message and exit.\n",
            "  --version     Output version information and exit.\n",
            "  --bind ADDR         Bind to the supplied IPv4/IPv6 address (default 0.0.0.0).\n",
            "  --ipv4             Restrict the listener to IPv4 sockets.\n",
            "  --ipv6             Restrict the listener to IPv6 sockets (defaults to :: when no bind address is provided).\n",
            "  --port PORT         Listen on the supplied TCP port (default 873).\n",
            "  --once              Accept a single connection and exit.\n",
            "  --max-sessions N    Accept N connections before exiting (N > 0).\n",
            "  --no-detach         Run in foreground (no-op; daemon doesn't fork by default).\n",
            "  --config FILE      Load module definitions from FILE (packages install {default_config}).\n",
            "  --module SPEC      Register an in-memory module (NAME=PATH[,COMMENT]).\n",
            "  --motd-file FILE   Append MOTD lines from FILE before module listings.\n",
            "  --motd-line TEXT   Append TEXT as an additional MOTD line.\n",
            "  --lock-file FILE   Track module connection limits across processes using FILE.\n",
            "  --pid-file FILE    Write the daemon PID to FILE for process supervision.\n",
            "  --bwlimit=RATE[:BURST]  Limit per-connection bandwidth in KiB/s.\n",
            "                          Optional :BURST caps the token bucket; 0 = unlimited.\n",
            "  --no-bwlimit       Remove any per-connection bandwidth limit configured so far.\n",
            "\n",
            "The listener accepts legacy @RSYNCD: connections sequentially, reports the\n",
            "negotiated protocol as 32, lists configured modules for #list requests, and\n",
            "replies with an @ERROR diagnostic while full module support is implemented.\n",
        ),
        program = program,
        version = manifest.rust_version(),
        source_line = source_line(),
        default_config = default_config,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_text_upstream_contains_program_name() {
        let text = help_text(Brand::Upstream);
        assert!(text.contains("rsync"));
    }

    #[test]
    fn help_text_oc_contains_program_name() {
        let text = help_text(Brand::Oc);
        assert!(text.contains("oc-rsync"));
    }

    #[test]
    fn help_text_contains_version() {
        let text = help_text(Brand::Upstream);
        // Should contain version information
        assert!(text.contains('.'));
    }

    #[test]
    fn help_text_contains_usage() {
        let text = help_text(Brand::Upstream);
        assert!(text.contains("Usage:"));
    }

    #[test]
    fn help_text_contains_help_option() {
        let text = help_text(Brand::Upstream);
        assert!(text.contains("--help"));
    }

    #[test]
    fn help_text_contains_version_option() {
        let text = help_text(Brand::Upstream);
        assert!(text.contains("--version"));
    }

    #[test]
    fn help_text_contains_port_option() {
        let text = help_text(Brand::Upstream);
        assert!(text.contains("--port"));
    }

    #[test]
    fn help_text_contains_config_option() {
        let text = help_text(Brand::Upstream);
        assert!(text.contains("--config"));
    }

    #[test]
    fn help_text_is_not_empty() {
        let text = help_text(Brand::Upstream);
        assert!(!text.is_empty());
    }
}
