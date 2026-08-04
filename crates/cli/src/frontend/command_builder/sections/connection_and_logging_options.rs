//! Clap definitions for connection, transport, and logging/output options.

use super::super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

/// Adds the connection and logging argument group to the command.
pub(crate) fn add_connection_and_logging_options(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("contimeout")
                .long("contimeout")
                .value_name("SECS")
                .help("Set connection timeout in seconds (0 disables the limit).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new())
                .overrides_with("no-contimeout"),
        )
        .arg(
            Arg::new("no-contimeout")
                .long("no-contimeout")
                .help("Disable connection timeout.")
                .action(ArgAction::SetTrue)
                .overrides_with("contimeout"),
        )
        .arg(
            Arg::new("protocol")
                .long("protocol")
                .value_name("NUM")
                .help("Force protocol version NUM when accessing rsync daemons.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new())
                .overrides_with("protocol"),
        )
        .arg(
            Arg::new("sockopts")
                .long("sockopts")
                .value_name("OPTIONS")
                .help("Set additional socket options (comma-separated list).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("tcp-fastopen")
                .long("tcp-fastopen")
                .help_heading("oc-rsync extensions")
                .value_name("MODE")
                .help(
                    "Enable TCP Fast Open on daemon and client sockets (auto, on, off). \
                     Default is auto: enabled where supported, skipped elsewhere.",
                )
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("blocking-io")
                .long("blocking-io")
                .help("Force the remote shell to use blocking I/O.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-blocking-io"),
        )
        .arg(
            Arg::new("no-blocking-io")
                .long("no-blocking-io")
                .help("Disable forced blocking I/O on the remote shell.")
                .action(ArgAction::SetTrue)
                .overrides_with("blocking-io"),
        )
        .arg(
            Arg::new("compress")
                .long("compress")
                .short('z')
                .help("Enable compression during transfers.")
                .action(ArgAction::Count)
                .overrides_with("no-compress"),
        )
        .arg(
            Arg::new("no-compress")
                .long("no-compress")
                .visible_alias("no-z")
                .help("Disable compression.")
                .action(ArgAction::SetTrue)
                .overrides_with("compress"),
        )
        .arg(
            Arg::new("compress-level")
                .long("compress-level")
                .visible_alias("zl")
                .value_name("LEVEL")
                .help("Set compression level (0-9). 0 disables compression.")
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("compress-choice")
                .long("compress-choice")
                .visible_alias("zc")
                .value_name("ALGO")
                .help("Select compression algorithm (e.g. zlib, zstd).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        // upstream: options.c --compress-threads / NEWS-3.4.2
        .arg(
            Arg::new("compress-threads")
                .long("compress-threads")
                .visible_alias("zt")
                .value_name("N")
                .help("Set zstd worker thread count (0 lets zstd choose).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("old-compress")
                .long("old-compress")
                .help("Use old-style (zlib) compression.")
                .action(ArgAction::SetTrue)
                .overrides_with("new-compress"),
        )
        .arg(
            Arg::new("new-compress")
                .long("new-compress")
                .help("Use new-style compression (typically zstd).")
                .action(ArgAction::SetTrue)
                .overrides_with("old-compress"),
        )
        .arg(
            Arg::new("skip-compress")
                .long("skip-compress")
                .value_name("LIST")
                .help("Skip compressing files with suffixes in LIST.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("open-noatime")
                .long("open-noatime")
                .help("Attempt to open source files without updating access times.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-open-noatime"),
        )
        .arg(
            Arg::new("no-open-noatime")
                .long("no-open-noatime")
                .help("Disable O_NOATIME handling when opening source files.")
                .action(ArgAction::SetTrue)
                .overrides_with("open-noatime"),
        )
        .arg(
            Arg::new("aes")
                .long("aes")
                .help_heading("oc-rsync extensions")
                .help("Force AES ciphers for oc's embedded ssh:// transport (ssh:// URL operands only) even without hardware AES detection; standard host:path SSH selects ciphers via ssh_config or the -e/--rsh remote shell.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-aes"),
        )
        .arg(
            Arg::new("no-aes")
                .long("no-aes")
                .help_heading("oc-rsync extensions")
                .help("Disable automatic AES cipher selection for oc's embedded ssh:// transport (ssh:// URL operands only); standard host:path SSH selects ciphers via ssh_config or the -e/--rsh remote shell.")
                .action(ArgAction::SetTrue)
                .overrides_with("aes"),
        )
        .arg(
            Arg::new("iconv")
                .long("iconv")
                .value_name("CONVERT_SPEC")
                .help(
                    "Convert filenames using iconv (use '.' for locale defaults or LOCAL,REMOTE charsets).",
                )
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new())
                .conflicts_with("no-iconv"),
        )
        .arg(
            Arg::new("no-iconv")
                .long("no-iconv")
                .help("Disable iconv charset conversion.")
                .action(ArgAction::SetTrue)
                .conflicts_with("iconv"),
        )
        .arg(
            Arg::new("stderr")
                .long("stderr")
                .value_name("MODE")
                .help("Change stderr output mode (errors, all, client).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("info")
                .long("info")
                .value_name("FLAGS")
                .help("Adjust informational messages; use --info=help for details.")
                .action(ArgAction::Append)
                .value_parser(OsStringValueParser::new())
                .value_delimiter(','),
        )
        .arg(
            Arg::new("debug")
                .long("debug")
                .value_name("FLAGS")
                .help("Adjust diagnostic output; use --debug=help for details.")
                .action(ArgAction::Append)
                .value_parser(OsStringValueParser::new())
                .value_delimiter(','),
        )
        .arg(
            Arg::new("dparam")
                .long("dparam")
                .value_name("OVERRIDE")
                .help("Override daemon config parameter (can be specified multiple times).")
                .action(ArgAction::Append)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("ssh-cipher")
                .long("ssh-cipher")
                .help_heading("oc-rsync extensions")
                .value_name("CIPHERS")
                .help("Comma-separated cipher preference list for oc's embedded ssh:// transport (ssh:// URL operands only); standard host:path SSH uses ssh_config Ciphers or the -e/--rsh remote shell.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("ssh-connect-timeout")
                .long("ssh-connect-timeout")
                .help_heading("oc-rsync extensions")
                .value_name("SECS")
                .help("Connection timeout in seconds for oc's embedded ssh:// transport (ssh:// URL operands only); standard host:path SSH uses ssh_config ConnectTimeout or the -e/--rsh remote shell.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("ssh-keepalive")
                .long("ssh-keepalive")
                .help_heading("oc-rsync extensions")
                .value_name("SECS")
                .help("Keepalive interval in seconds for oc's embedded ssh:// transport (ssh:// URL operands only; 0 = disable); standard host:path SSH uses ssh_config ServerAliveInterval or the -e/--rsh remote shell.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("ssh-identity")
                .long("ssh-identity")
                .help_heading("oc-rsync extensions")
                .value_name("FILE")
                .help("Identity file for oc's embedded ssh:// transport (ssh:// URL operands only; repeatable); standard host:path SSH uses ssh_config IdentityFile or the -e/--rsh remote shell.")
                .action(ArgAction::Append)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("ssh-no-agent")
                .long("ssh-no-agent")
                .help_heading("oc-rsync extensions")
                .help("Disable SSH agent authentication for oc's embedded ssh:// transport (ssh:// URL operands only); standard host:path SSH follows ssh_config or the -e/--rsh remote shell.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("ssh-strict-host-key-checking")
                .long("ssh-strict-host-key-checking")
                .help_heading("oc-rsync extensions")
                .value_name("MODE")
                .help("Host key verification policy (yes, no, ask) for oc's embedded ssh:// transport (ssh:// URL operands only); standard host:path SSH uses ssh_config StrictHostKeyChecking or the -e/--rsh remote shell.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("ssh-ipv6")
                .long("ssh-ipv6")
                .help_heading("oc-rsync extensions")
                .help("Prefer IPv6 for oc's embedded ssh:// transport (ssh:// URL operands only); rsync's own --ipv4/--ipv6 cover daemon and other transports.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("ssh-port")
                .long("ssh-port")
                .help_heading("oc-rsync extensions")
                .value_name("PORT")
                .help("Port override for oc's embedded ssh:// transport (ssh:// URL operands only); standard host:path SSH sets the port via ssh_config Port or the -e/--rsh remote shell.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("jump-host")
                .long("jump-host")
                .help_heading("oc-rsync extensions")
                .value_name("[user@]HOST[:PORT][,...]")
                .help("Comma-separated ProxyJump hosts for oc's embedded ssh:// transport (ssh:// URL operands only); standard host:path SSH configures ProxyJump via ssh_config or the -e/--rsh remote shell.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("args")
                .action(ArgAction::Append)
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
                .value_parser(OsStringValueParser::new()),
        )
}
