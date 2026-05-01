//! Transfer behavior arguments: archive, recursive, dirs, inc-recursive,
//! relative, one-file-system, implied-dirs, checksum, size-only, ignore-times,
//! ignore-existing, existing, update, modify-window, sparse, fuzzy, force,
//! qsort, mkpath, prune-empty-dirs, partial, and delay-updates.

use super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

/// Adds transfer behavior and file selection flags to the command.
pub(super) fn add_transfer_args(command: ClapCommand) -> ClapCommand {
    command
        .arg(
            Arg::new("archive")
                .long("archive")
                .short('a')
                .help("Enable archive mode (implies --owner, --group, --perms, and --times).")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("recursive")
                .long("recursive")
                .short('r')
                .help("Recurse into directories when processing source operands.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-recursive"),
        )
        .arg(
            Arg::new("no-recursive")
                .long("no-recursive")
                .visible_alias("no-r")
                .help("Do not recurse into directories when processing source operands.")
                .action(ArgAction::SetTrue)
                .overrides_with("recursive"),
        )
        .arg(
            Arg::new("inc-recursive")
                .long("inc-recursive")
                .visible_alias("i-r")
                .help("Scan directories incrementally during recursion (default behaviour).")
                .action(ArgAction::SetTrue)
                .overrides_with("no-inc-recursive"),
        )
        .arg(
            Arg::new("no-inc-recursive")
                .long("no-inc-recursive")
                .visible_alias("no-i-r")
                .help("Disable incremental directory scanning during recursion.")
                .action(ArgAction::SetTrue)
                .overrides_with("inc-recursive"),
        )
        .arg(
            Arg::new("dirs")
                .long("dirs")
                .short('d')
                .help("Copy directory entries even when recursion is disabled.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-dirs"),
        )
        .arg(
            Arg::new("no-dirs")
                .long("no-dirs")
                .visible_alias("no-d")
                .help("Skip directory entries when recursion is disabled.")
                .action(ArgAction::SetTrue)
                .overrides_with("dirs"),
        )
        .arg(
            Arg::new("relative")
                .long("relative")
                .short('R')
                .help("Preserve source path components relative to the current directory.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-relative"),
        )
        .arg(
            Arg::new("no-relative")
                .long("no-relative")
                .visible_alias("no-R")
                .help("Disable preservation of source path components.")
                .action(ArgAction::SetTrue)
                .overrides_with("relative"),
        )
        .arg(
            Arg::new("one-file-system")
                .long("one-file-system")
                .short('x')
                .help("Do not cross filesystem boundaries during traversal. Specify twice (-xx) to also skip root-level mount points.")
                .action(ArgAction::Count)
                .overrides_with("no-one-file-system"),
        )
        .arg(
            Arg::new("no-one-file-system")
                .long("no-one-file-system")
                .visible_alias("no-x")
                .help("Allow traversal across filesystem boundaries.")
                .action(ArgAction::SetTrue)
                .overrides_with("one-file-system"),
        )
        .arg(
            Arg::new("implied-dirs")
                .long("implied-dirs")
                .visible_alias("i-d")
                .help("Create parent directories implied by source paths.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-implied-dirs"),
        )
        .arg(
            Arg::new("no-implied-dirs")
                .long("no-implied-dirs")
                .visible_alias("no-i-d")
                .help("Disable creation of parent directories implied by source paths.")
                .action(ArgAction::SetTrue)
                .overrides_with("implied-dirs"),
        )
        .arg(
            Arg::new("checksum")
                .long("checksum")
                .short('c')
                .help("Skip files whose contents already match by checksum.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-checksum"),
        )
        .arg(
            Arg::new("no-checksum")
                .long("no-checksum")
                .visible_alias("no-c")
                .help("Disable checksum-based change detection.")
                .action(ArgAction::SetTrue)
                .overrides_with("checksum"),
        )
        .arg(
            Arg::new("checksum-choice")
                .long("checksum-choice")
                .visible_alias("cc")
                .value_name("ALGO")
                .help(
                    "Select the strong checksum algorithm (auto, md4, md5, xxh64, xxh3, or xxh128).",
                )
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("checksum-seed")
                .long("checksum-seed")
                .value_name("NUM")
                .help("Set the checksum seed used by xxhash-based algorithms.")
                .num_args(1)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("size-only")
                .long("size-only")
                .help("Skip files whose size already matches the destination.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("ignore-times")
                .long("ignore-times")
                .short('I')
                .help("Disable quick checks based on size and modification time.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("ignore-existing")
                .long("ignore-existing")
                .help("Skip updating files that already exist at the destination.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("existing")
                .long("existing")
                .visible_alias("ignore-non-existing")
                .help("Skip creating new files that do not already exist at the destination.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("update")
                .long("update")
                .short('u')
                .help("Skip files that are newer on the destination.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("modify-window")
                .long("modify-window")
                .value_name("SECS")
                .help("Treat mtimes within SECS seconds as equal when comparing files.")
                .num_args(1)
                .action(ArgAction::Set)
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("sparse")
                .long("sparse")
                .short('S')
                .help("Preserve sparse files by creating holes in the destination.")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-sparse"),
        )
        .arg(
            Arg::new("no-sparse")
                .long("no-sparse")
                .visible_alias("no-S")
                .help("Disable sparse file handling.")
                .action(ArgAction::SetTrue)
                .conflicts_with("sparse"),
        )
        .arg(
            Arg::new("fuzzy")
                .long("fuzzy")
                .short('y')
                .help("Search for basis files with similar names. Specify twice (-yy) to also search reference directories.")
                .action(ArgAction::Count)
                .overrides_with("no-fuzzy"),
        )
        .arg(
            Arg::new("no-fuzzy")
                .long("no-fuzzy")
                .visible_alias("no-y")
                .help("Disable --fuzzy basis file search.")
                .action(ArgAction::SetTrue)
                .overrides_with("fuzzy"),
        )
        .arg(
            Arg::new("force")
                .long("force")
                .help("Remove conflicting destination directories to make way for files.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-force"),
        )
        .arg(
            Arg::new("no-force")
                .long("no-force")
                .help("Preserve conflicting destination directories when updating entries.")
                .action(ArgAction::SetTrue)
                .overrides_with("force"),
        )
        .arg(
            Arg::new("qsort")
                .long("qsort")
                .help("Use qsort instead of merge sort for file list sorting.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("inc-recursive-send")
                .long("inc-recursive-send")
                .help(
                    "Opt-in: advertise INC_RECURSE on the sender capability string \
                     for interop testing. Sender-side incremental recursion has not \
                     been validated against upstream rsync; default off.",
                )
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("mkpath")
                .long("mkpath")
                .help("Create destination's missing path components.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-mkpath"),
        )
        .arg(
            Arg::new("no-mkpath")
                .long("no-mkpath")
                .visible_alias("old-dirs")
                .visible_alias("old-d")
                .help("Disable creation of destination path components (compatibility with older rsync releases).")
                .action(ArgAction::SetTrue)
                .overrides_with("mkpath"),
        )
        .arg(
            Arg::new("prune-empty-dirs")
                .long("prune-empty-dirs")
                .short('m')
                .help("Skip creating directories that remain empty after filters.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-prune-empty-dirs"),
        )
        .arg(
            Arg::new("no-prune-empty-dirs")
                .long("no-prune-empty-dirs")
                .visible_alias("no-m")
                .help("Disable pruning of empty directories.")
                .action(ArgAction::SetTrue)
                .overrides_with("prune-empty-dirs"),
        )
        .arg(
            Arg::new("partial")
                .long("partial")
                .help("Keep partially transferred files on error.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-partial"),
        )
        .arg(
            Arg::new("no-partial")
                .long("no-partial")
                .help("Discard partially transferred files on error.")
                .action(ArgAction::SetTrue)
                .overrides_with("partial"),
        )
        .arg(
            Arg::new("delay-updates")
                .long("delay-updates")
                .help("Put all updated files into place at end of transfer.")
                .action(ArgAction::SetTrue)
                .overrides_with("no-delay-updates"),
        )
        .arg(
            Arg::new("no-delay-updates")
                .long("no-delay-updates")
                .help("Write updated files immediately during the transfer.")
                .action(ArgAction::SetTrue)
                .overrides_with("delay-updates"),
        )
}
