use super::super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

pub(crate) fn build_base_command(program_name: &'static str) -> ClapCommand {
    ClapCommand::new(program_name)
            .disable_help_flag(true)
            .disable_version_flag(true)
            .arg_required_else_help(false)
            .arg(
                Arg::new("help")
                    .long("help")
                    .help("Show this help message and exit.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("human-readable")
                    .short('h')
                    .long("human-readable")
                    .value_name("LEVEL")
                    .help(
                        "Output numbers in a human-readable format; optional LEVEL selects 0, 1, or 2.",
                    )
                    .num_args(0..=1)
                    .default_missing_value("1")
                    .require_equals(true)
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Set)
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
                Arg::new("msgs2stderr")
                    .long("msgs2stderr")
                    .help("Send messages to standard error instead of standard output.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-msgs2stderr"),
            )
            .arg(
                Arg::new("8-bit-output")
                    .long("8-bit-output")
                    .help("Leave high-bit characters unescaped in output.")
                    .action(ArgAction::SetTrue),
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
                Arg::new("max-alloc")
                    .long("max-alloc")
                    .value_name("SIZE")
                    .help("Limit memory allocation to SIZE bytes (can use K, M, G suffixes).")
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
                Arg::new("version")
                    .long("version")
                    .short('V')
                    .help("Output version information and exit.")
                    .action(ArgAction::SetTrue),
            )
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
                    .help("Prefer IPv4 when contacting remote hosts.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("ipv6"),
            )
            .arg(
                Arg::new("ipv6")
                    .long("ipv6")
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
                    .help("Disable pruning of empty directories.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("prune-empty-dirs"),
            )
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
                    .alias("cc")
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
                    .help("Search for basis files with similar names at the destination.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-fuzzy"),
            )
            .arg(
                Arg::new("no-fuzzy")
                    .long("no-fuzzy")
                    .help("Disable --fuzzy basis file search.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("fuzzy"),
            )
            .arg(
                Arg::new("links")
                    .long("links")
                    .short('l')
                    .help("Copy symlinks as symlinks.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with_all(["copy-links", "no-links"]),
            )
            .arg(
                Arg::new("no-links")
                    .long("no-links")
                    .visible_alias("no-l")
                    .help("Do not copy symlinks as symlinks.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with_all(["links", "copy-links"]),
            )
            .arg(
                Arg::new("copy-links")
                    .long("copy-links")
                    .short('L')
                    .help("Transform symlinks into referent files/directories.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("no-copy-links"),
            )
            .arg(
                Arg::new("copy-unsafe-links")
                    .long("copy-unsafe-links")
                    .help("Transform unsafe symlinks into referent files/directories.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("no-copy-unsafe-links"),
            )
            .arg(
                Arg::new("hard-links")
                    .long("hard-links")
                    .short('H')
                    .help("Preserve hard links between files.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("no-hard-links"),
            )
            .arg(
                Arg::new("copy-dirlinks")
                    .long("copy-dirlinks")
                    .short('k')
                    .help("Transform symlinked directories into referent directories.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("keep-dirlinks")
                    .long("keep-dirlinks")
                    .short('K')
                    .help("Treat existing destination symlinks to directories as directories.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("no-keep-dirlinks"),
            )
            .arg(
                Arg::new("no-copy-links")
                    .long("no-copy-links")
                    .help("Preserve symlinks instead of following them.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("copy-links"),
            )
            .arg(
                Arg::new("no-copy-unsafe-links")
                    .long("no-copy-unsafe-links")
                    .help("Preserve unsafe symlinks instead of following them.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("copy-unsafe-links"),
            )
            .arg(
                Arg::new("no-hard-links")
                    .long("no-hard-links")
                    .visible_alias("no-H")
                    .help("Disable hard link preservation.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("hard-links"),
            )
            .arg(
                Arg::new("safe-links")
                    .long("safe-links")
                    .help("Skip symlinks that point outside the transfer root.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("munge-links")
                    .long("munge-links")
                    .help("Munge symlinks to make them safe in daemon mode.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-munge-links"),
            )
            .arg(
                Arg::new("no-munge-links")
                    .long("no-munge-links")
                    .help("Disable symlink munging.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("munge-links"),
            )
            .arg(
                Arg::new("trust-sender")
                    .long("trust-sender")
                    .help("Trust the sender's file list without additional verification.")
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
                Arg::new("no-keep-dirlinks")
                    .long("no-keep-dirlinks")
                    .help("Disable treating destination symlinks to directories as directories.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("keep-dirlinks"),
            )
            .arg(
                Arg::new("archive-devices")
                    .short('D')
                    .help("Preserve device and special files (equivalent to --devices --specials).")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-archive-devices"),
            )
            .arg(
                Arg::new("no-archive-devices")
                    .long("no-D")
                    .visible_alias("no-archive-devices")
                    .help("Disable preservation of device and special files (negates -D).")
                    .action(ArgAction::SetTrue)
                    .overrides_with("archive-devices"),
            )
            .arg(
                Arg::new("devices")
                    .long("devices")
                    .help("Preserve device files.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("no-devices"),
            )
            .arg(
                Arg::new("copy-devices")
                    .long("copy-devices")
                    .help("Copy device files as regular files, transferring their contents.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("write-devices")
                    .long("write-devices")
                    .help("Write file data directly to device files instead of creating nodes.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("no-write-devices"),
            )
            .arg(
                Arg::new("no-devices")
                    .long("no-devices")
                    .help("Disable device file preservation.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("devices"),
            )
            .arg(
                Arg::new("no-write-devices")
                    .long("no-write-devices")
                    .help("Do not write file data directly to device files.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("write-devices"),
            )
            .arg(
                Arg::new("specials")
                    .long("specials")
                    .help("Preserve special files such as FIFOs.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("no-specials"),
            )
            .arg(
                Arg::new("no-specials")
                    .long("no-specials")
                    .help("Disable preservation of special files such as FIFOs.")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("specials"),
            )
            .arg(
                Arg::new("super")
                    .long("super")
                    .help(
                        "Receiver attempts super-user activities (implies --owner, --group, and --perms).",
                    )
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-super"),
            )
            .arg(
                Arg::new("no-super")
                    .long("no-super")
                    .help("Disable super-user handling even when running as root.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("super"),
            )
            .arg(
                Arg::new("fake-super")
                    .long("fake-super")
                    .help("Store/restore privileged attrs using xattrs instead of real permissions.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-fake-super"),
            )
            .arg(
                Arg::new("no-fake-super")
                    .long("no-fake-super")
                    .help("Disable fake super-user mode.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("fake-super"),
            )
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
                    .help("Do not cross filesystem boundaries during traversal.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-one-file-system"),
            )
            .arg(
                Arg::new("no-one-file-system")
                    .long("no-one-file-system")
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
