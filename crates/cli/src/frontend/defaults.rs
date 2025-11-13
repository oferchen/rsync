//! Constants shared across the CLI front-end.

use time::{format_description::FormatItem, macros::format_description};

/// Comma-separated description of the options currently recognised by the CLI help text.
pub(super) const SUPPORTED_OPTIONS_LIST: &str = concat!(
    "--help, --version/-V, -e/--rsh, --rsync-path, --connect-program, --port, --address, ",
    "--remote-option/-M, --protect-args/-s, --no-protect-args, --secluded-args, --no-secluded-args, ",
    "--ipv4, --ipv6, --daemon, --dry-run/-n, --list-only, --archive/-a, --recursive/-r, --no-recursive, ",
    "--dirs/-d, --no-dirs, --delete/--del, --delete-before, --delete-during, --delete-delay, --delete-after, ",
    "--delete-excluded, --max-delete, --min-size, --max-size, --block-size, --backup/-b, --backup-dir, ",
    "--suffix, --checksum/-c, --checksum-choice, --checksum-seed, --size-only, --ignore-times, --ignore-existing, --existing, ",
    "--ignore-missing-args, --delete-missing-args, --update/-u, --modify-window, --exclude, --exclude-from, ",
    "--include, --include-from, --compare-dest, --copy-dest, --link-dest, --hard-links/-H, --no-hard-links, ",
    "--cvs-exclude/-C, --filter/-F (including exclude-if-present=FILE), --files-from, --password-file, --no-motd, ",
    "--from0, --no-from0, --bwlimit, --no-bwlimit, --timeout, --contimeout, --stop-after/--time-limit, --stop-at, --sockopts, ",
    "--blocking-io, --no-blocking-io, --protocol, --compress/-z, --no-compress, --compress-level, --compress-choice, ",
    "--skip-compress, --open-noatime, --no-open-noatime, --iconv, --no-iconv, --info, --debug, --verbose/-v, ",
    "--relative/-R, --no-relative, --one-file-system/-x, --no-one-file-system, --implied-dirs, --no-implied-dirs, ",
    "--mkpath, --prune-empty-dirs/-m, --no-prune-empty-dirs, --progress, --no-progress, --msgs2stderr, --outbuf, ",
    "--itemize-changes/-i, --out-format, --stats, --partial, --no-partial, --partial-dir, --temp-dir, --log-file, ",
    "--log-file-format, --delay-updates, --no-delay-updates, --whole-file/-W, --no-whole-file, --remove-source-files, ",
    "--remove-sent-files, --append, --no-append, --append-verify, --preallocate, --fsync, --inplace, --no-inplace, ",
    "--human-readable/-h, --no-human-readable, -P, --sparse/-S, --no-sparse, --links/-l, --no-links/--no-l, ",
    "--copy-links/-L, --no-copy-links, ",
    "--copy-unsafe-links, --no-copy-unsafe-links, --safe-links, --copy-dirlinks/-k, --keep-dirlinks/-K, --no-keep-dirlinks, ",
    "-D, --devices, --copy-devices, --no-devices, --specials, --no-specials, --super, --no-super, --owner, --no-owner, --group, --no-group, ",
    "--chown, --usermap, --groupmap, --chmod, --perms/-p, --no-perms, --times/-t, --no-times, --omit-dir-times, ",
    "--no-omit-dir-times, --omit-link-times, --no-omit-link-times, --acls/-A, --no-acls, --xattrs/-X, --no-xattrs, ",
    "--numeric-ids, --no-numeric-ids"
);

/// Format string used when forwarding `--itemize-changes` to fallback binaries.
pub(super) const ITEMIZE_CHANGES_FORMAT: &str = "%i %n%L";

/// Default patterns excluded by `--cvs-exclude`.
pub(super) const CVS_EXCLUDE_PATTERNS: &[&str] = &[
    "RCS",
    "SCCS",
    "CVS",
    "CVS.adm",
    "RCSLOG",
    "cvslog.*",
    "tags",
    "TAGS",
    ".make.state",
    ".nse_depinfo",
    "*~",
    "#*",
    ".#*",
    ",*",
    "_$*",
    "*$",
    "*.old",
    "*.bak",
    "*.BAK",
    "*.orig",
    "*.rej",
    ".del-*",
    "*.a",
    "*.olb",
    "*.o",
    "*.obj",
    "*.so",
    "*.exe",
    "*.Z",
    "*.elc",
    "*.ln",
    "core",
    ".svn/",
    ".git/",
    ".hg/",
    ".bzr/",
];

/// Timestamp format used for `--list-only` and `--out-format` placeholders.
pub(crate) const LIST_TIMESTAMP_FORMAT: &[FormatItem<'static>] = format_description!(
    "[year]/[month padding:zero]/[day padding:zero] [hour padding:zero]:[minute padding:zero]:[second padding:zero]"
);
