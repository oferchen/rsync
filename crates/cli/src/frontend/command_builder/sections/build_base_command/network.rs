//! Network and remote shell arguments: rsh, rsync-path, connect-program, port,
//! remote-option, protect-args, old-args, ipv4/ipv6, address, and max-alloc.

use super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

/// Adds network, remote shell, and connection flags to the command.
pub(super) fn add_network_args(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("rsh")
                .long("rsh")
                .short('e')
                .value_name("COMMAND")
                .help("Use remote shell COMMAND for remote transfers.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("rsync-path")
                .long("rsync-path")
                .value_name("PROGRAM")
                .help("Use PROGRAM as the remote rsync executable during remote transfers.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("connect-program")
                .long("connect-program")
                .value_name("COMMAND")
                .help(
                    "Execute COMMAND to reach rsync:// daemons (supports %H and %P placeholders).",
                )
                .num_args(1)
                .action(ArgAction::Set)
                .allow_hyphen_values(true)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("port")
                .long("port")
                .value_name("PORT")
                .help("Use PORT as the default rsync:// daemon TCP port when none is specified.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u16)),
        )
        .arg(
            Arg::new("remote-option")
                .long("remote-option")
                .short('M')
                .value_name("OPTION")
                .help("Forward OPTION to the remote rsync command.")
                .action(ArgAction::Append)
                .num_args(1)
                .allow_hyphen_values(true)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("protect-args")
                .long("protect-args")
                .short('s')
                .alias("secluded-args")
                .help("Protect remote shell arguments from expansion.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-protect-args"),
        )
        .arg(
            Arg::new("no-protect-args")
                .long("no-protect-args")
                .visible_alias("no-s")
                .alias("no-secluded-args")
                .help("Allow the remote shell to expand wildcard arguments.")
                .action(ArgAction::SetTrue)
                .overrides_with("protect-args"),
        )
        .arg(
            Arg::new("old-args")
                .long("old-args")
                .help("Use old-style argument handling (pre-3.2.4 behavior).")
                .action(ArgAction::SetTrue)
                .overrides_with("no-old-args"),
        )
        .arg(
            Arg::new("no-old-args")
                .long("no-old-args")
                .help("Use new-style argument handling (default).")
                .action(ArgAction::SetTrue)
                .overrides_with("old-args"),
        )
        .arg(
            Arg::new("ipv4")
                .long("ipv4")
                .short('4')
                .help("Prefer IPv4 when contacting remote hosts.")
                .action(ArgAction::SetTrue)
                .conflicts_with("ipv6"),
        )
        .arg(
            Arg::new("ipv6")
                .long("ipv6")
                .short('6')
                .help("Prefer IPv6 when contacting remote hosts.")
                .action(ArgAction::SetTrue)
                .conflicts_with("ipv4"),
        )
        .arg(
            Arg::new("address")
                .long("address")
                .value_name("ADDRESS")
                .help("Bind outgoing connections to ADDRESS when contacting remotes.")
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("max-alloc")
                .long("max-alloc")
                .value_name("SIZE")
                .help("Limit memory allocation to SIZE bytes (can use K, M, G suffixes).")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
        )
}
