//! The server argv, read the way 3.5.1's single popt pass reads it.
//!
//! upstream: options.c:1515 parses the whole argv in one pass, and the first
//! `--server` restarts it (options.c:1517-1524). Every oc reader of a server
//! argv - the remote-shell server and the daemon - goes through
//! [`normalize_server_argv`] so each sees the same options.

use std::ffi::{OsStr, OsString};

mod long_options;

pub use long_options::{LongOption, long_option};

/// The bare `--` that ends option processing.
///
/// upstream: options.c:1515 - the server runs the same popt parser as the
/// client (`poptGetContext(RSYNC_NAME, argc, argv, long_options, 0)`), and popt
/// consumes a bare `--` and hands back every following argv entry as a plain
/// operand, whatever it looks like.
///
/// `support/rrsync:655` builds its command as
/// `(RSYNC, *rsync_opts, '--', '.', *rsync_args)` and depends on precisely that
/// property; its comment at `:558` states it outright: "an arg that starts with
/// a '-' is safe due to our use of '--' in the cmd tuple".
const END_OF_OPTIONS: &str = "--";

/// Splits a server argument list at the end-of-options marker.
///
/// Returns `(options, operands)`. The marker itself is consumed, as popt
/// consumes it. When no bare `--` is present the whole list is options and
/// `operands` is empty, so a peer that never sends the marker is parsed exactly
/// as before.
///
/// Every scanner over the server argv applies this for itself rather than
/// trusting its caller. Upstream gets the property once, from a single popt
/// pass; oc walks the same argv from three places
/// (`detect_secluded_args_flag`, `parse_server_long_flags`, and
/// `parse_server_flag_string_and_args`), and one of them forgetting is exactly
/// the defect this closes - a path operand that looks like an option would be
/// obeyed as one.
pub fn split_at_end_of_options(args: &[OsString]) -> (&[OsString], &[OsString]) {
    let marker = OsStr::new(END_OF_OPTIONS);
    match args.iter().position(|arg| arg.as_os_str() == marker) {
        Some(at) => (&args[..at], &args[at + 1..]),
        None => (args, &[]),
    }
}

/// A server argv rejected the way 3.5.1's popt pass rejects it.
///
/// upstream: main.c:1913-1916 - a failed parse_arguments() is option_error()
/// (options.c:907-918, `"rsync: %s"`) then `exit_cleanup(RERR_SYNTAX)`. The
/// trailer names the process `server` only once `--server` has been parsed:
/// am_server is set by that option (options.c:889), so an option refused before
/// it reports `[client=...]`. popt's own errors also gain `on remote machine: `
/// under am_server (options.c:2051-2054); rsync's own refusals never do.
#[derive(Debug, PartialEq, Eq)]
pub struct ArgvRefusal {
    /// The refusal text, without the program-name prefix.
    pub text: String,
    /// Whether `--server` preceded the refused token.
    pub after_server: bool,
}

impl ArgvRefusal {
    /// popt's own refusal of `token`: `on remote machine: ` is prefixed once
    /// `--server` was seen (options.c:2051-2054).
    #[must_use]
    pub fn popt(token: &str, error: &str, after_server: bool) -> Self {
        let prefix = if after_server {
            "on remote machine: "
        } else {
            ""
        };
        Self {
            text: format!("{prefix}{token}: {error}"),
            after_server,
        }
    }
}

/// Rewrites a server argv into the form the server's argv scanners read.
///
/// The server parses its whole argv with one popt pass (options.c:1497), so a
/// short-option cluster counts wherever it sits, before `--server` included.
/// Each cluster is read as popt reads it: letters are options up to the first
/// one that takes a value ([`SHORT_OPTIONS_WITH_VALUE`]), whose value is the
/// rest of the token or else the next token. The rewrite puts every option
/// letter, in order, into one compact flag string right after `--server`, with
/// the last `-e` value (upstream's `shell_cmd`, compat.c:166) appended, so the
/// flag-string parser and the capability decoder read it exactly as they read
/// a single cluster.
///
/// - `-M` values are only recorded on a server (options.c:1762-1777; only a
///   local copy's forked server re-parses them, pipe.c:143-150), so they are
///   dropped, and so is `--remote-option`.
/// - `-B`, `-@` and `-T` become the `-B<n>`, `-@<n>` and `--temp-dir=<dir>`
///   tokens the long-option pass reads.
/// - Filter options (`-f`, `-F`, `--filter`, `--exclude`, `--include` and
///   the `-from` pair) become joined `--opt=VALUE` tokens behind the flag
///   string, in argv order, each `-F` already expanded to its rule. Those ahead
///   of `--server` are kept twice: upstream's restarted pass parses them again
///   without clearing `filter_list` or `F_option_cnt`.
/// - A long option of 3.5.2's table ([`long_option`]) that names a short
///   switch joins the flag string as that letter; a `--no-` negation of one
///   removes the letter; `--block-size` and `--rsh` are read as `-B` and
///   `-e`; `--chown` becomes the `--usermap` / `--groupmap` it implies. Any
///   other value-taking long option is joined to its value, so every later
///   pass reads one `--opt=VALUE` token.
///
/// Tokens after the `--` marker are operands and pass through untouched. The
/// rewrite is idempotent, so it can run again over a secluded-args list.
///
/// # Errors
///
/// Returns 3.5.1's refusal for an unknown short letter, a value-taking option
/// with no value, or a remote option that does not start with a dash
/// (options.c:1764-1767).
pub fn normalize_server_argv(args: &[OsString]) -> Result<Vec<OsString>, ArgvRefusal> {
    let (options, operands) = split_at_end_of_options(args);
    let mut kept = Vec::with_capacity(args.len() + 1);
    let mut letters = String::new();
    let mut rsh: Option<String> = None;
    let mut server_at: Option<usize> = None;
    let mut first_cluster_at: Option<usize> = None;
    let mut after_server = false;
    let mut filters: Vec<ArgvFilter> = Vec::new();
    let mut id_maps = IdMaps::default();
    let mut tokens = options.iter().enumerate();
    while let Some((index, arg)) = tokens.next() {
        let text = arg.to_string_lossy();
        if index == 0 || !text.starts_with('-') || text == "-" {
            kept.push(arg.clone());
            continue;
        }
        if text == "--server" {
            // upstream: options.c:1535-1542 - the first `--server` restarts
            // the popt pass from argv[1], so every option ahead of it is
            // parsed twice and a counting letter (`-v`, `-X`, `-x`) counts
            // twice.
            if !after_server {
                letters.push_str(&letters.clone());
                filters.extend_from_within(..);
            }
            after_server = true;
            kept.push(arg.clone());
            server_at.get_or_insert(kept.len());
            continue;
        }
        if is_two_arg_server_long_flag(&text) {
            kept.push(arg.clone());
            if let Some((_, value)) = tokens.next() {
                kept.push(value.clone());
            }
            continue;
        }
        if FILTER_OPTIONS.contains(&&*text) {
            let (_, value) = tokens
                .next()
                .ok_or_else(|| ArgvRefusal::popt(&text, "missing argument", after_server))?;
            let mut joined = OsString::from(format!("{text}="));
            joined.push(value);
            filters.push(ArgvFilter::Option(joined));
            continue;
        }
        if is_joined_filter_option(&text) {
            filters.push(ArgvFilter::Option(arg.clone()));
            continue;
        }
        if text == "--remote-option" {
            let value = tokens
                .next()
                .map(|(_, value)| value.to_string_lossy().into_owned())
                .ok_or_else(|| ArgvRefusal::popt(&text, "missing argument", after_server))?;
            check_remote_option(&value, after_server)?;
            continue;
        }
        if let Some(value) = text.strip_prefix("--remote-option=") {
            check_remote_option(value, after_server)?;
            continue;
        }
        if let Some(rest) = text.strip_prefix("--") {
            let (name, joined) = match rest.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (rest, None),
            };
            let kind = long_option(name);
            // popt binds a value-taking option's value from the next token
            // when it is not joined; join it so every later pass reads one.
            let mut token = arg.clone();
            if kind.is_some_and(LongOption::takes_value) && joined.is_none() {
                let (_, value) = tokens
                    .next()
                    .ok_or_else(|| ArgvRefusal::popt(&text, "missing argument", after_server))?;
                token = OsString::from(format!("{text}="));
                token.push(value);
            }
            let value = || {
                let token = token.to_string_lossy();
                token
                    .split_once('=')
                    .map_or_else(String::new, |(_, v)| v.to_owned())
            };
            match kind {
                Some(LongOption::Letter(letter)) if joined.is_none() => {
                    first_cluster_at.get_or_insert(kept.len());
                    letters.push(letter);
                }
                Some(LongOption::Clears(letter)) if joined.is_none() => {
                    clear_letter(&mut letters, letter);
                }
                Some(LongOption::ValueLetter(letter)) => {
                    let value = value();
                    match letter {
                        'e' => rsh = Some(value),
                        _ => kept.push(OsString::from(format!("-{letter}{value}"))),
                    }
                }
                _ if name == "chown" => {
                    let value = value();
                    kept.extend(id_maps.chown(&value, &mut letters, after_server)?);
                }
                _ if name == "usermap" || name == "groupmap" => {
                    let side = if name == "usermap" {
                        IdSide::User
                    } else {
                        IdSide::Group
                    };
                    id_maps.map(side, &mut letters, after_server)?;
                    kept.push(token);
                }
                _ => kept.push(token),
            }
            continue;
        }
        first_cluster_at.get_or_insert(kept.len());
        let cluster = &text[1..];
        for (offset, letter) in cluster.char_indices() {
            if !SHORT_OPTIONS.contains(letter) {
                return Err(ArgvRefusal::popt(&text, "unknown option", after_server));
            }
            if letter == 'F' {
                filters.push(ArgvFilter::RsyncFilter);
                continue;
            }
            if !SHORT_OPTIONS_WITH_VALUE.contains(&letter) {
                letters.push(letter);
                continue;
            }
            let rest = &cluster[offset + letter.len_utf8()..];
            let value = if rest.is_empty() {
                tokens
                    .next()
                    .map(|(_, value)| value.to_string_lossy().into_owned())
                    .ok_or_else(|| ArgvRefusal::popt(&text, "missing argument", after_server))?
            } else {
                rest.to_owned()
            };
            match letter {
                'e' => rsh = Some(value),
                'M' => check_remote_option(&value, after_server)?,
                'B' => kept.push(OsString::from(format!("-B{value}"))),
                '@' => kept.push(OsString::from(format!("-@{value}"))),
                'T' => kept.push(OsString::from(format!("--temp-dir={value}"))),
                // upstream: options.c:753 `-f`, OPT_FILTER at :1620.
                _ => filters.push(ArgvFilter::Option(OsString::from(format!(
                    "--filter={value}"
                )))),
            }
            break;
        }
    }
    let mut inserted = Vec::new();
    if !letters.is_empty() || rsh.is_some() {
        let mut flag_string = format!("-{letters}");
        if let Some(value) = rsh {
            flag_string.push('e');
            flag_string.push_str(&value);
        }
        inserted.push(OsString::from(flag_string));
    }
    let mut rsync_filter_count = 0;
    inserted.extend(filters.into_iter().filter_map(|filter| match filter {
        ArgvFilter::Option(token) => Some(token),
        ArgvFilter::RsyncFilter => {
            rsync_filter_count += 1;
            rsync_filter_rule(rsync_filter_count).map(OsString::from)
        }
    }));
    // Right after `--server`, where the flag-string parser looks.
    let at = server_at.or(first_cluster_at).unwrap_or(kept.len());
    kept.splice(at..at, inserted);
    if options.len() < args.len() {
        kept.push(OsString::from(END_OF_OPTIONS));
        kept.extend(operands.iter().cloned());
    }
    Ok(kept)
}

/// A filter option from the server argv, held in argv order until the
/// `-F` occurrences can be numbered.
#[derive(Clone)]
enum ArgvFilter {
    /// A joined `--opt=VALUE` filter token.
    Option(OsString),
    /// One `-F`.
    RsyncFilter,
}

/// The long filter options, each taking one value.
///
/// upstream: options.c:1647-1683 - OPT_FILTER, OPT_EXCLUDE, OPT_INCLUDE,
/// OPT_EXCLUDE_FROM and OPT_INCLUDE_FROM.
pub const FILTER_OPTIONS: [&str; 5] = [
    "--filter",
    "--exclude",
    "--include",
    "--exclude-from",
    "--include-from",
];

/// Whether `arg` is a filter option with its value joined by `=`.
pub fn is_joined_filter_option(arg: &str) -> bool {
    FILTER_OPTIONS.iter().any(|name| {
        arg.strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('='))
    })
}

/// The rule the `count`th `-F` adds; a third and later add nothing.
///
/// upstream: options.c:1754-1763.
fn rsync_filter_rule(count: usize) -> Option<String> {
    match count {
        1 => Some("--filter=: /.rsync-filter".to_owned()),
        2 => Some("--filter=- .rsync-filter".to_owned()),
        _ => None,
    }
}

/// The letters `-a` sets, each in its own variable.
///
/// upstream: options.c:1686-1702 `case 'a'`.
const ARCHIVE_LETTERS: &str = "rlptgoD";

/// Resets what every earlier `letter` set, as a `--no-` option does.
///
/// A `-a` is expanded first when it set `letter`: popt sets each of its
/// variables separately, so `-a --no-p` keeps the rest of the archive set.
fn clear_letter(letters: &mut String, letter: char) {
    if ARCHIVE_LETTERS.contains(letter) && letters.contains('a') {
        *letters = letters.replace('a', ARCHIVE_LETTERS);
    }
    letters.retain(|set| set != letter);
}

/// Which id map an option sets.
#[derive(Clone, Copy)]
enum IdSide {
    User,
    Group,
}

impl IdSide {
    const fn option(self) -> &'static str {
        match self {
            Self::User => "--usermap",
            Self::Group => "--groupmap",
        }
    }

    /// Sets `preserve_uid` / `preserve_gid`, a switch, so a letter already
    /// in the flag string is not repeated.
    fn preserve(self, letters: &mut String) {
        let letter = match self {
            Self::User => 'o',
            Self::Group => 'g',
        };
        if !letters.contains(letter) {
            letters.push(letter);
        }
    }
}

/// upstream's `usermap` / `groupmap` and their `_via_chown` flags, as the
/// pass has set them so far: `None` while unset, else whether `--chown` set
/// it.
#[derive(Default)]
struct IdMaps {
    user: Option<bool>,
    group: Option<bool>,
}

impl IdMaps {
    fn slot(&mut self, side: IdSide) -> &mut Option<bool> {
        match side {
            IdSide::User => &mut self.user,
            IdSide::Group => &mut self.group,
        }
    }

    /// upstream: options.c:1929-1959 `case OPT_USERMAP` / `OPT_GROUPMAP` - a
    /// second map is refused, and the map sets `preserve_uid` / `_gid`.
    fn map(
        &mut self,
        side: IdSide,
        letters: &mut String,
        after_server: bool,
    ) -> Result<(), ArgvRefusal> {
        let option = side.option();
        let text = match *self.slot(side) {
            Some(true) => Some(format!("{option} conflicts with prior --chown.")),
            Some(false) => Some(format!("You can only specify {option} once.")),
            None => None,
        };
        refuse(text, after_server)?;
        *self.slot(side) = Some(false);
        side.preserve(letters);
        Ok(())
    }

    /// upstream: options.c:1961-2000 `case OPT_CHOWN` - the part before the
    /// first `:` becomes `usermap = "*:USER"` when non-empty, the part after
    /// it `groupmap = "*:GROUP"`; each refuses a map set before it and sets
    /// `preserve_uid` / `_gid`. Returns the `--usermap` / `--groupmap` tokens
    /// the long-option pass reads.
    fn chown(
        &mut self,
        spec: &str,
        letters: &mut String,
        after_server: bool,
    ) -> Result<Vec<OsString>, ArgvRefusal> {
        let (user, group) = match spec.split_once(':') {
            Some((user, group)) => (user, Some(group)),
            None => (spec, None),
        };
        let mut maps = Vec::new();
        if !user.is_empty() {
            self.chown_side(IdSide::User, user, letters, after_server, &mut maps)?;
        }
        if let Some(group) = group.filter(|group| !group.is_empty()) {
            self.chown_side(IdSide::Group, group, letters, after_server, &mut maps)?;
        }
        Ok(maps)
    }

    fn chown_side(
        &mut self,
        side: IdSide,
        name: &str,
        letters: &mut String,
        after_server: bool,
        maps: &mut Vec<OsString>,
    ) -> Result<(), ArgvRefusal> {
        let option = side.option();
        let affecting = match side {
            IdSide::User => "user",
            IdSide::Group => "group",
        };
        let text = match *self.slot(side) {
            Some(false) => Some(format!("--chown conflicts with prior {option}.")),
            Some(true) => Some(format!(
                "You can only specify a {affecting}-affecting --chown once."
            )),
            None => None,
        };
        refuse(text, after_server)?;
        *self.slot(side) = Some(true);
        side.preserve(letters);
        maps.push(OsString::from(format!("{option}=*:{name}")));
        Ok(())
    }
}

/// rsync's own refusal of `text`, if any: no `on remote machine: ` prefix
/// (options.c:907-918 `option_error()`).
fn refuse(text: Option<String>, after_server: bool) -> Result<(), ArgvRefusal> {
    match text {
        Some(text) => Err(ArgvRefusal { text, after_server }),
        None => Ok(()),
    }
}

/// Refuses a remote-option value without a leading dash, as options.c:1764-1767
/// does.
fn check_remote_option(value: &str, after_server: bool) -> Result<(), ArgvRefusal> {
    if value.starts_with('-') {
        Ok(())
    } else {
        Err(ArgvRefusal {
            text: format!("Remote option must start with a dash: {value}"),
            after_server,
        })
    }
}

/// Every short option letter in 3.5.1's popt table.
///
/// upstream: options.c long_options[], named entries plus the unnamed `-D`,
/// `-F` and `-P` aliases. A letter outside this set is popt's
/// `unknown option`.
const SHORT_OPTIONS: &str = "0468@ABCDEFHIJKLMNOPRSTUVWXabcdefghiklmnopqrstuvxyz";

/// The short options whose value follows in the same or the next token.
///
/// upstream: options.c long_options[] - `-@` (modify-window), `-B`
/// (block-size), `-M` (remote-option), `-T` (temp-dir), `-e` (rsh) and `-f`
/// (filter) are the only short options with `POPT_ARG_STRING`/`POPT_ARG_INT`.
pub const SHORT_OPTIONS_WITH_VALUE: [char; 6] = ['@', 'B', 'M', 'T', 'e', 'f'];

/// Returns `true` when the argument is a bare server-mode long flag whose
/// value is supplied as the next argv slot (popt `POPT_ARG_STRING` arity).
///
/// Upstream `options.c::server_options()` emits a handful of path-bearing
/// long flags as two argv slots: the flag name and the value, joined later
/// by `safe_arg("", value)`. The split form is used unconditionally for
/// these flags, independent of `protect_args`. Without this awareness the
/// flag name is treated as the first positional path and the value as the
/// second, so the alt-dest interop test (`--copy-dest /path/alt3 . /path/to/`)
/// lands the source files under `$HOME/--copy-dest/` instead of creating
/// the dest root at `/path/to/`.
///
/// This is the long-option arm of the arity model documented above; the
/// operand parser consults it BEFORE `is_known_server_long_flag` so the
/// value slot is consumed rather than the flag name alone. `--partial-dir`
/// is included here (also `POPT_ARG_STRING`), replacing its former one-off
/// `idx += 2` special case in the operand parser. The structured
/// `parse_server_long_flags` pass keeps its own explicit `--partial-dir` arm
/// (it stores the value), which matches before this predicate is consulted.
///
/// # Upstream Reference
///
/// - `options.c:837` - `-e`/`--rsh` is `POPT_ARG_STRING` (the short arm).
/// - `options.c:2981-2985` - `--backup-dir`
/// - `options.c:3062-3066` - `--partial-dir`
/// - `options.c:3106-3109` - `--temp-dir`
/// - `options.c:3119-3122` - `--copy-dest` / `--link-dest` / `--compare-dest`
///   (via `alt_dest_opt(0)`)
/// - `options.c:3143-3146` - `--files-from`
pub fn is_two_arg_server_long_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--compare-dest"
            | "--copy-dest"
            | "--link-dest"
            | "--backup-dir"
            | "--temp-dir"
            | "--files-from"
            | "--partial-dir"
            | "--nice"
    )
}

#[cfg(test)]
mod tests;
