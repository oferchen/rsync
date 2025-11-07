//! Constants shared across the CLI front-end.

use time::{format_description::FormatItem, macros::format_description};

/// Comma-separated description of the options currently recognised by the CLI help text.
pub(super) const SUPPORTED_OPTIONS_LIST: &str = "--help, --human-readable/-h, --no-human-readable, --version/-V, --daemon, --dry-run/-n, --list-only, --archive/-a, --recursive/-r, --delete/--del, --delete-before, --delete-during, --delete-delay, --delete-after, --max-delete, --min-size, --max-size, --block-size, --checksum/-c, --checksum-choice, --checksum-seed, --size-only, --ignore-existing, --ignore-missing-args, --modify-window, --delay-updates, --exclude, --exclude-from, --include, --include-from, --compare-dest, --copy-dest, --link-dest, --filter (including exclude-if-present=FILE) and -F, --files-from, --password-file, --no-motd, --from0, --bwlimit, --no-bwlimit, --timeout, --contimeout, --protocol, --rsync-path, --port, --connect-program, --remote-option/-M, --ipv4, --ipv6, --compress/-z, --no-compress, --compress-level, --compress-choice, --skip-compress, --info, --debug, --verbose/-v, --progress, --no-progress, --msgs2stderr, --outbuf, --itemize-changes/-i, --out-format, --stats, --partial, --partial-dir, --temp-dir, --log-file, --log-file-format, --no-partial, --remove-source-files, --remove-sent-files, --inplace, --no-inplace, --whole-file/-W, --no-whole-file, -P, --sparse/-S, --no-sparse, --copy-links/-L, --no-copy-links, --copy-unsafe-links, --no-copy-unsafe-links, --copy-dirlinks/-k, --keep-dirlinks/-K, --no-keep-dirlinks, -D, --devices, --no-devices, --specials, --no-specials, --super, --no-super, --owner, --no-owner, --group, --no-group, --chown, --usermap, --groupmap, --chmod, --perms/-p, --no-perms, --times/-t, --no-times, --omit-dir-times, --no-omit-dir-times, --omit-link-times, --no-omit-link-times, --acls/-A, --no-acls, --xattrs/-X, --no-xattrs, --numeric-ids, --one-file-system/-x, --no-one-file-system, --mkpath, and --no-numeric-ids";

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
