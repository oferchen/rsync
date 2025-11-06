use super::super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

pub(crate) fn section_02(command: ClapCommand) -> ClapCommand {
    command
            .arg(
                Arg::new("partial-dir")
                    .long("partial-dir")
                    .value_name("DIR")
                    .help("Store partially transferred files in DIR.")
                    .value_parser(OsStringValueParser::new())
                    .overrides_with("no-partial"),
            )
            .arg(
                Arg::new("temp-dir")
                    .long("temp-dir")
                    .visible_alias("tmp-dir")
                    .value_name("DIR")
                    .help("Store temporary files in DIR while transferring.")
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("whole-file")
                    .long("whole-file")
                    .short('W')
                    .help("Copy files without using the delta-transfer algorithm.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-whole-file"),
            )
            .arg(
                Arg::new("no-whole-file")
                    .long("no-whole-file")
                    .help("Enable the delta-transfer algorithm (disable whole-file copies).")
                    .action(ArgAction::SetTrue)
                    .overrides_with("whole-file"),
            )
            .arg(
                Arg::new("remove-source-files")
                    .long("remove-source-files")
                    .help("Remove source files after a successful transfer.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("remove-sent-files"),
            )
            .arg(
                Arg::new("remove-sent-files")
                    .long("remove-sent-files")
                    .help("Alias of --remove-source-files.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("remove-source-files"),
            )
            .arg(
                Arg::new("append")
                    .long("append")
                    .help(
                        "Append data to existing destination files without rewriting preserved bytes.",
                    )
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-append")
                    .overrides_with("append-verify"),
            )
            .arg(
                Arg::new("no-append")
                    .long("no-append")
                    .help("Disable append mode for destination updates.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("append")
                    .overrides_with("append-verify"),
            )
            .arg(
                Arg::new("append-verify")
                    .long("append-verify")
                    .help("Append data while verifying that existing bytes match the sender.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("append")
                    .overrides_with("no-append"),
            )
            .arg(
                Arg::new("preallocate")
                    .long("preallocate")
                    .help("Preallocate destination files before writing.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("inplace")
                    .long("inplace")
                    .help("Write updated data directly to destination files.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-inplace"),
            )
            .arg(
                Arg::new("no-inplace")
                    .long("no-inplace")
                    .help("Use temporary files when updating regular files.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("inplace"),
            )
            .arg(
                Arg::new("partial-progress")
                    .short('P')
                    .help("Equivalent to --partial --progress.")
                    .action(ArgAction::Count)
                    .overrides_with("no-partial")
                    .overrides_with("no-progress"),
            )
            .arg(
                Arg::new("delete")
                    .long("delete")
                    .visible_alias("del")
                    .help("Remove destination files that are absent from the source.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("delete-before")
                    .long("delete-before")
                    .help("Remove destination files that are absent from the source before transfers start.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("delete-during")
                    .long("delete-during")
                    .help("Remove destination files that are absent from the source during directory traversal.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("delete-delay")
                    .long("delete-delay")
                    .help("Compute deletions during the transfer and prune them once the run completes.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("delete-after")
                    .long("delete-after")
                    .help("Remove destination files after transfers complete.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("ignore-missing-args")
                    .long("ignore-missing-args")
                    .help("Skip missing source arguments without reporting an error.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("delete-excluded")
                    .long("delete-excluded")
                    .help("Remove excluded destination files during deletion sweeps.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("max-delete")
                    .long("max-delete")
                    .value_name("NUM")
                    .help("Limit the number of deletions that may occur.")
                    .num_args(1)
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("min-size")
                    .long("min-size")
                    .value_name("SIZE")
                    .help("Skip files smaller than the specified size.")
                    .num_args(1)
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("max-size")
                    .long("max-size")
                    .value_name("SIZE")
                    .help("Skip files larger than the specified size.")
                    .num_args(1)
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("block-size")
                    .long("block-size")
                    .value_name("SIZE")
                    .help("Force the delta-transfer block size to SIZE bytes.")
                    .num_args(1)
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("backup")
                    .long("backup")
                    .short('b')
                    .help("Create backups before overwriting or deleting existing entries.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("backup-dir")
                    .long("backup-dir")
                    .value_name("DIR")
                    .help("Store backups inside DIR instead of alongside the destination.")
                    .num_args(1)
                    .action(ArgAction::Set)
                    .allow_hyphen_values(true)
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("suffix")
                    .long("suffix")
                    .value_name("SUFFIX")
                    .help("Append SUFFIX to backup names (default '~').")
                    .num_args(1)
                    .action(ArgAction::Set)
                    .allow_hyphen_values(true)
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("exclude")
                    .long("exclude")
                    .value_name("PATTERN")
                    .help("Skip files matching PATTERN.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("exclude-from")
                    .long("exclude-from")
                    .value_name("FILE")
                    .help("Read exclude patterns from FILE.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("include")
                    .long("include")
                    .value_name("PATTERN")
                    .help("Re-include files matching PATTERN after exclusions.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("include-from")
                    .long("include-from")
                    .value_name("FILE")
                    .help("Read include patterns from FILE.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("compare-dest")
                    .long("compare-dest")
                    .value_name("DIR")
                    .help("Skip creating destination files that match DIR.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("copy-dest")
                    .long("copy-dest")
                    .value_name("DIR")
                    .help("Copy matching files from DIR instead of the source.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("link-dest")
                    .long("link-dest")
                    .value_name("DIR")
                    .help("Hard-link matching files from DIR into the destination.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("cvs-exclude")
                    .long("cvs-exclude")
                    .short('C')
                    .help("Auto-ignore files using CVS-style ignore rules.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("filter")
                    .long("filter")
                    .value_name("RULE")
                    .help("Apply filter RULE (supports '+' include, '-' exclude, '!' clear, 'protect PATTERN', 'risk PATTERN', 'merge[,MODS] FILE' or '.[,MODS] FILE', and 'dir-merge[,MODS] FILE' or ':[,MODS] FILE').")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("rsync-filter")
                    .short('F')
                    .help("Shortcut for per-directory .rsync-filter handling (repeat to also load receiver-side files).")
                    .action(ArgAction::Count),
            )
            .arg(
                Arg::new("files-from")
                    .long("files-from")
                    .value_name("FILE")
                    .help("Read additional source operands from FILE.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("password-file")
                    .long("password-file")
                    .value_name("FILE")
                    .help("Read daemon passwords from FILE when contacting rsync:// daemons.")
                    .value_parser(OsStringValueParser::new())
                    .action(ArgAction::Set),
            )
            .arg(
                Arg::new("no-motd")
                    .long("no-motd")
                    .help("Suppress daemon MOTD lines when listing rsync:// modules.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("from0")
                    .long("from0")
                    .help("Treat file list entries as NUL-terminated records.")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("owner")
                    .long("owner")
                    .short('o')
                    .help("Preserve file ownership (requires super-user).")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-owner"),
            )
            .arg(
                Arg::new("no-owner")
                    .long("no-owner")
                    .help("Disable ownership preservation.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("owner"),
            )
            .arg(
                Arg::new("group")
                    .long("group")
                    .short('g')
                    .help("Preserve file group (requires suitable privileges).")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-group"),
            )
            .arg(
                Arg::new("no-group")
                    .long("no-group")
                    .help("Disable group preservation.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("group"),
            )
            .arg(
                Arg::new("chown")
                    .long("chown")
                    .value_name("USER:GROUP")
                    .help("Set destination ownership to USER and/or GROUP.")
                    .value_parser(OsStringValueParser::new())
                    .num_args(1),
            )
            .arg(
                Arg::new("chmod")
                    .long("chmod")
                    .value_name("SPEC")
                    .help("Apply chmod-style SPEC modifiers to received files.")
                    .action(ArgAction::Append)
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("perms")
                    .long("perms")
                    .short('p')
                    .help("Preserve file permissions.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-perms"),
            )
            .arg(
                Arg::new("no-perms")
                    .long("no-perms")
                    .help("Disable permission preservation.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("perms"),
            )
            .arg(
                Arg::new("times")
                    .long("times")
                    .short('t')
                    .help("Preserve modification times.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-times"),
            )
            .arg(
                Arg::new("no-times")
                    .long("no-times")
                    .help("Disable modification time preservation.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("times"),
            )
            .arg(
                Arg::new("omit-dir-times")
                    .long("omit-dir-times")
                    .short('O')
                    .help("Skip preserving directory modification times.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-omit-dir-times"),
            )
            .arg(
                Arg::new("no-omit-dir-times")
                    .long("no-omit-dir-times")
                    .help("Preserve directory modification times.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("omit-dir-times"),
            )
            .arg(
                Arg::new("omit-link-times")
                    .long("omit-link-times")
                    .help("Skip preserving symlink modification times.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-omit-link-times"),
            )
            .arg(
                Arg::new("no-omit-link-times")
                    .long("no-omit-link-times")
                    .help("Preserve symlink modification times.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("omit-link-times"),
            )
            .arg(
                Arg::new("acls")
                    .long("acls")
                    .short('A')
                    .help("Preserve POSIX ACLs when supported.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-acls"),
            )
            .arg(
                Arg::new("no-acls")
                    .long("no-acls")
                    .help("Disable POSIX ACL preservation.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("acls"),
            )
            .arg(
                Arg::new("xattrs")
                    .long("xattrs")
                    .short('X')
                    .help("Preserve extended attributes when supported.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-xattrs"),
            )
            .arg(
                Arg::new("no-xattrs")
                    .long("no-xattrs")
                    .help("Disable extended attribute preservation.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("xattrs"),
            )
            .arg(
                Arg::new("numeric-ids")
                    .long("numeric-ids")
                    .help("Preserve numeric UID/GID values.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("no-numeric-ids"),
            )
            .arg(
                Arg::new("no-numeric-ids")
                    .long("no-numeric-ids")
                    .help("Map UID/GID values to names when possible.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("numeric-ids"),
            )
            .arg(
                Arg::new("bwlimit")
                    .long("bwlimit")
                    .value_name("RATE")
                    .help("Limit I/O bandwidth in KiB/s (0 disables the limit).")
                    .num_args(1)
                    .action(ArgAction::Set)
                    .overrides_with("no-bwlimit")
                    .value_parser(OsStringValueParser::new()),
            )
            .arg(
                Arg::new("no-bwlimit")
                    .long("no-bwlimit")
                    .help("Disable any configured bandwidth limit.")
                    .action(ArgAction::SetTrue)
                    .overrides_with("bwlimit"),
            )
            .arg(
                Arg::new("timeout")
                    .long("timeout")
                    .value_name("SECS")
                    .help("Set I/O timeout in seconds (0 disables the timeout).")
                    .num_args(1)
                    .action(ArgAction::Set)
                    .value_parser(OsStringValueParser::new()),
            )
}
