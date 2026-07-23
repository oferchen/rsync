use std::collections::VecDeque;
use std::ffi::OsString;

use clap::ArgMatches;

/// A single filter directive captured in command-line (argv) order.
///
/// upstream: options.c feeds each `--include`/`--exclude`/`--filter`/
/// `--include-from`/`--exclude-from`/`-C`/`-F` to `parse_filter_str`,
/// `parse_filter_file`, or the CVS defaults at the argv position where it
/// appears. Rule evaluation is strictly first-match-wins over encounter order
/// (exclude.c:parse_filter_str appends each rule to the tail of the list), so
/// the CLI must preserve command-line order across every filter-producing
/// option rather than grouping includes ahead of excludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilterOrderToken {
    /// `--exclude PATTERN`
    Exclude(OsString),
    /// `--include PATTERN`
    Include(OsString),
    /// `--exclude-from FILE`
    ExcludeFrom(OsString),
    /// `--include-from FILE`
    IncludeFrom(OsString),
    /// `--filter RULE` / `-f RULE` (also the directive an `-F` expands to)
    Filter(OsString),
    /// `--cvs-exclude` / `-C`
    CvsExclude,
    /// `--apple-double-skip`
    AppleDoubleSkip,
}

/// Builds the filter directive stream in true command-line (argv) order.
///
/// The argv scan runs over the short-option-expanded arguments and records
/// every filter-producing option in the order it appears. Parsed values come
/// from clap (so `--opt=VALUE`, `-f VALUE`, and non-UTF-8 paths are handled
/// exactly as clap parsed them); the scan supplies only the ordering, which
/// clap cannot express for the repeatable `-F` flag. The resulting stream is
/// evaluated first-match-wins, mirroring how upstream options.c dispatches each
/// option at its argv position.
pub(crate) fn build_filter_order(matches: &ArgMatches, args: &[OsString]) -> Vec<FilterOrderToken> {
    let mut excludes = value_queue(matches, "exclude");
    let mut includes = value_queue(matches, "include");
    let mut exclude_from = value_queue(matches, "exclude-from");
    let mut include_from = value_queue(matches, "include-from");
    let mut filters = value_queue(matches, "filter");

    let mut tokens = Vec::new();
    let mut f_occurrence = 0usize;
    let mut after_double_dash = false;
    let mut expect_value = false;

    for arg in args.iter().skip(1) {
        if after_double_dash {
            break;
        }
        if expect_value {
            expect_value = false;
            continue;
        }

        // Option names are ASCII; only values (which are skipped via
        // `expect_value` or the inline `=VALUE`) can be non-UTF-8, so a lossy
        // conversion is safe for recognizing which option a token is.
        let text = arg.to_string_lossy();

        if text == "--" {
            after_double_dash = true;
            continue;
        }

        if let Some(token) = long_value_option(&text, &mut excludes, &mut includes)
            .or_else(|| long_from_option(&text, &mut exclude_from, &mut include_from))
        {
            expect_value = true;
            if let Some(token) = token {
                tokens.push(token);
            }
            continue;
        }

        if text == "--filter" || text == "-f" {
            expect_value = true;
            if let Some(value) = filters.pop_front() {
                tokens.push(FilterOrderToken::Filter(value));
            }
            continue;
        }

        if let Some(token) = long_inline_option(&text, &mut excludes, &mut includes)
            .or_else(|| inline_from_option(&text, &mut exclude_from, &mut include_from))
            .or_else(|| inline_filter_option(&text, &mut filters))
        {
            if let Some(token) = token {
                tokens.push(token);
            }
            continue;
        }

        if text == "--cvs-exclude" {
            tokens.push(FilterOrderToken::CvsExclude);
            continue;
        }
        if text == "--apple-double-skip" {
            tokens.push(FilterOrderToken::AppleDoubleSkip);
            continue;
        }

        // Short-option cluster: `-F` and `-C` may ride alongside other flags.
        // upstream: options.c:1605-1608 - each `-F` expands to a filter
        // directive (first: dir-merge, second: exclude) at its argv position.
        if text.starts_with('-') && !text.starts_with("--") && text.len() > 1 {
            for ch in text[1..].chars() {
                match ch {
                    'F' => {
                        if let Some(directive) = rsync_filter_shortcut_directive(f_occurrence) {
                            tokens.push(FilterOrderToken::Filter(directive));
                        }
                        f_occurrence += 1;
                    }
                    'C' => tokens.push(FilterOrderToken::CvsExclude),
                    _ => {}
                }
            }
        }
    }

    tokens
}

/// Collects a value-bearing option's parsed values in argv order.
fn value_queue(matches: &ArgMatches, id: &str) -> VecDeque<OsString> {
    matches
        .get_many::<OsString>(id)
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

/// Recognizes `--exclude`/`--include` (value in the following token).
fn long_value_option(
    text: &str,
    excludes: &mut VecDeque<OsString>,
    includes: &mut VecDeque<OsString>,
) -> Option<Option<FilterOrderToken>> {
    match text {
        "--exclude" => Some(excludes.pop_front().map(FilterOrderToken::Exclude)),
        "--include" => Some(includes.pop_front().map(FilterOrderToken::Include)),
        _ => None,
    }
}

/// Recognizes `--exclude-from`/`--include-from` (value in the following token).
fn long_from_option(
    text: &str,
    exclude_from: &mut VecDeque<OsString>,
    include_from: &mut VecDeque<OsString>,
) -> Option<Option<FilterOrderToken>> {
    match text {
        "--exclude-from" => Some(exclude_from.pop_front().map(FilterOrderToken::ExcludeFrom)),
        "--include-from" => Some(include_from.pop_front().map(FilterOrderToken::IncludeFrom)),
        _ => None,
    }
}

/// Recognizes `--exclude=VALUE`/`--include=VALUE` (inline value).
fn long_inline_option(
    text: &str,
    excludes: &mut VecDeque<OsString>,
    includes: &mut VecDeque<OsString>,
) -> Option<Option<FilterOrderToken>> {
    if text.starts_with("--exclude=") {
        Some(excludes.pop_front().map(FilterOrderToken::Exclude))
    } else if text.starts_with("--include=") {
        Some(includes.pop_front().map(FilterOrderToken::Include))
    } else {
        None
    }
}

/// Recognizes `--exclude-from=VALUE`/`--include-from=VALUE` (inline value).
fn inline_from_option(
    text: &str,
    exclude_from: &mut VecDeque<OsString>,
    include_from: &mut VecDeque<OsString>,
) -> Option<Option<FilterOrderToken>> {
    if text.starts_with("--exclude-from=") {
        Some(exclude_from.pop_front().map(FilterOrderToken::ExcludeFrom))
    } else if text.starts_with("--include-from=") {
        Some(include_from.pop_front().map(FilterOrderToken::IncludeFrom))
    } else {
        None
    }
}

/// Recognizes `--filter=VALUE` (inline value).
fn inline_filter_option(
    text: &str,
    filters: &mut VecDeque<OsString>,
) -> Option<Option<FilterOrderToken>> {
    text.starts_with("--filter=")
        .then(|| filters.pop_front().map(FilterOrderToken::Filter))
}

/// Maps an `-F` occurrence to the filter directive it expands to.
///
/// upstream: options.c:1608 - the first `-F` adds `: /.rsync-filter`
/// (dir-merge), the second adds `- .rsync-filter` (exclude); third and later
/// occurrences are ignored.
fn rsync_filter_shortcut_directive(occurrence: usize) -> Option<OsString> {
    match occurrence {
        0 => Some(OsString::from("dir-merge /.rsync-filter")),
        1 => Some(OsString::from("exclude .rsync-filter")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsync_filter_shortcut_directive_maps_first_two_occurrences() {
        assert_eq!(
            rsync_filter_shortcut_directive(0),
            Some(OsString::from("dir-merge /.rsync-filter"))
        );
        assert_eq!(
            rsync_filter_shortcut_directive(1),
            Some(OsString::from("exclude .rsync-filter"))
        );
        assert_eq!(rsync_filter_shortcut_directive(2), None);
    }
}
