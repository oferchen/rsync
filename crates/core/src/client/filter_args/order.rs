use std::ffi::OsString;

/// A single filter directive captured in command-line (argv) order.
///
/// upstream: options.c feeds each `--include`/`--exclude`/`--filter`/
/// `--include-from`/`--exclude-from`/`-C`/`-F` to `parse_filter_str`,
/// `parse_filter_file`, or the CVS defaults at the argv position where it
/// appears. Rule evaluation is strictly first-match-wins over encounter order
/// (exclude.c:add_rule() appends each parsed rule to the tail of the list), so
/// the CLI must preserve command-line order across every filter-producing
/// option rather than grouping includes ahead of excludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOrderToken {
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
    /// `--delete-excluded`, recorded at its argv position because only the
    /// rules parsed after it carry `FILTRULE_SENDER_SIDE`
    /// (upstream: exclude.c:1478-1481 parse_rule_tok()).
    DeleteExcluded,
}
