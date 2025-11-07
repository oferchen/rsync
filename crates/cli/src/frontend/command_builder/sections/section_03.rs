use super::super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

pub(crate) fn section_03(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("contimeout")
                .long("contimeout")
                .value_name("SECS")
                .help("Set connection timeout in seconds (0 disables the limit).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("protocol")
                .long("protocol")
                .value_name("NUM")
                .help("Force protocol version NUM when accessing rsync daemons.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("compress")
                .long("compress")
                .short('z')
                .help("Enable compression during transfers.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-compress"),
        )
        .arg(
            Arg::new("no-compress")
                .long("no-compress")
                .help("Disable compression.")
                .action(ArgAction::SetTrue)
                .overrides_with("compress"),
        )
        .arg(
            Arg::new("compress-level")
                .long("compress-level")
                .value_name("LEVEL")
                .help("Set compression level (0 disables compression).")
                .help("Set compression level (0-9). 0 disables compression.")
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("compress-choice")
                .long("compress-choice")
                .value_name("ALGO")
                .help("Select compression algorithm (e.g. zlib, zstd).")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
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
            Arg::new("args")
                .action(ArgAction::Append)
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
                .value_parser(OsStringValueParser::new()),
        )
}
