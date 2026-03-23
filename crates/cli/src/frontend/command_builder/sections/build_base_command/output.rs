//! Output formatting and verbosity arguments: verbose, quiet, human-readable,
//! 8-bit-output, msgs2stderr, outbuf, itemize-changes, out-format, progress,
//! and stats.

use super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

/// Adds output formatting and verbosity flags to the command.
pub(super) fn add_output_args(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .short('v')
                .help("Increase verbosity; may be supplied multiple times.")
                .action(ArgAction::Count)
                .overrides_with("no-verbose")
                .overrides_with("quiet"),
        )
        .arg(
            Arg::new("no-verbose")
                .long("no-verbose")
                .visible_alias("no-v")
                .help("Disable verbosity (equivalent to --quiet).")
                .action(ArgAction::SetTrue)
                .overrides_with("verbose"),
        )
        .arg(
            Arg::new("quiet")
                .long("quiet")
                .short('q')
                .help("Suppress non-error messages.")
                .action(ArgAction::SetTrue)
                .overrides_with("verbose"),
        )
        .arg(
            Arg::new("human-readable")
                .short('h')
                .long("human-readable")
                .value_name("LEVEL")
                .help(
                    "Output numbers in a human-readable format; optional LEVEL selects 0, 1, or 2. Can be repeated to increase level.",
                )
                .num_args(0..=1)
                .default_missing_value("__h_count__")
                .require_equals(true)
                .value_parser(OsStringValueParser::new())
                .action(ArgAction::Append)
                .overrides_with("no-human-readable"),
        )
        .arg(
            Arg::new("no-human-readable")
                .long("no-human-readable")
                .visible_alias("no-h")
                .help("Disable human-readable number formatting.")
                .action(ArgAction::SetTrue)
                .overrides_with("human-readable"),
        )
        .arg(
            Arg::new("8-bit-output")
                .long("8-bit-output")
                .short('8')
                .help("Leave high-bit characters unescaped in output.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-8-bit-output"),
        )
        .arg(
            Arg::new("no-8-bit-output")
                .long("no-8-bit-output")
                .visible_alias("no-8")
                .help("Escape high-bit characters in output.")
                .action(ArgAction::SetTrue)
                .overrides_with("8-bit-output"),
        )
        .arg(
            Arg::new("msgs2stderr")
                .long("msgs2stderr")
                .help("Send messages to standard error instead of standard output.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-msgs2stderr"),
        )
        .arg(
            Arg::new("no-msgs2stderr")
                .long("no-msgs2stderr")
                .help("Send messages to standard output instead of standard error.")
                .action(ArgAction::SetTrue)
                .overrides_with("msgs2stderr"),
        )
        .arg(
            Arg::new("outbuf")
                .long("outbuf")
                .value_name("MODE")
                .help("Set stdout buffering to MODE (accepts N, L, or B).")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("itemize-changes")
                .long("itemize-changes")
                .short('i')
                .help("Output a change summary for each updated entry.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-itemize-changes"),
        )
        .arg(
            Arg::new("no-itemize-changes")
                .long("no-itemize-changes")
                .visible_alias("no-i")
                .help("Disable change summaries for updated entries.")
                .action(ArgAction::SetTrue)
                .overrides_with("itemize-changes"),
        )
        .arg(
            Arg::new("out-format")
                .long("out-format")
                .visible_alias("log-format")
                .value_name("FORMAT")
                .help("Customise transfer output using FORMAT for each processed entry.")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("progress")
                .long("progress")
                .help("Show progress information during transfers.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-progress"),
        )
        .arg(
            Arg::new("no-progress")
                .long("no-progress")
                .help("Disable progress reporting.")
                .action(ArgAction::SetTrue)
                .overrides_with("progress"),
        )
        .arg(
            Arg::new("stats")
                .long("stats")
                .help("Output transfer statistics after completion.")
                .action(ArgAction::SetTrue),
        )
}
