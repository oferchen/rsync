//! Core program identity and mode arguments: help, version, server, sender,
//! daemon, config, dry-run, and list-only.

use super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

/// Adds core program identity and mode flags to the command.
pub(super) fn add_core_args(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("help")
                .long("help")
                .help("Show this help message and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("version")
                .long("version")
                .short('V')
                .help("Output version information and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("server")
                .long("server")
                .help("Run in server mode (for remote rsync invocation).")
                .action(ArgAction::SetTrue)
                .hide(true),
        )
        .arg(
            Arg::new("sender")
                .long("sender")
                .help("Mark this process as the sender role (used with --server).")
                .action(ArgAction::SetTrue)
                .hide(true),
        )
        .arg(
            Arg::new("daemon")
                .long("daemon")
                .help("Run as an rsync daemon, serving files to rsync clients.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("config")
                .long("config")
                .value_name("FILE")
                .help("Specify alternate daemon config file (default: /etc/oc-rsyncd/oc-rsyncd.conf).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("detach")
                .long("detach")
                .help("Detach from the terminal and run as a background daemon.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-detach"),
        )
        .arg(
            Arg::new("no-detach")
                .long("no-detach")
                .help("Do not detach from the terminal (run daemon in foreground).")
                .action(ArgAction::SetTrue)
                .overrides_with("detach"),
        )
        .arg(
            Arg::new("dry-run")
                .long("dry-run")
                .short('n')
                .help("Validate transfers without modifying the destination.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("list-only")
                .long("list-only")
                .help("List files without performing a transfer.")
                .action(ArgAction::SetTrue),
        )
}
