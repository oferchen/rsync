use super::FilterFileSource;
use super::apple_double::append_apple_double_exclude_rules;
use super::cvs::append_cvs_exclude_rules;
use super::filter_str::{ParseContext, Template, Xflags, parse_filter_str};
use super::order::FilterOrderToken;
use super::sources::append_filter_rules_from_files;
use crate::client::{FilterRuleKind, FilterRuleSpec};
use crate::message::Message;

/// `--include`/`--exclude`: XFLG_OLD_PREFIXES alone (options.c:1625-1633).
const OLD_PREFIXES: Xflags = Xflags {
    old_prefixes: true,
    abs_if_slash: false,
    dir2wild3: false,
};

/// The template an argv option parses against: one rule, not word-split.
const fn argument(kind: FilterRuleKind) -> Template {
    Template {
        kind,
        word_split: false,
    }
}

/// Filter configuration supplied by the command line.
///
/// The ordered token stream preserves the argv position of every
/// filter-producing option so evaluation is first-match-wins over encounter
/// order, mirroring upstream options.c.
pub struct FilterInputs {
    /// The filter options in argv order.
    pub order: Vec<FilterOrderToken>,
    /// `--from0`/`-0`: makes `--exclude-from`/`--include-from` files
    /// NUL-delimited (upstream exclude.c:1774 parse_filter_file() eol_nulls).
    pub from0: bool,
}

/// Builds the rules a run's filter options add, in argv order.
///
/// The client and an rsh server share this: upstream parses both argvs in the
/// same `parse_arguments()` (options.c:1620-1656, :1718-1727), relative to the
/// process's cwd. Rules are appended in command-line order so the filter engine's
/// first-match-wins evaluation matches upstream rsync, where each
/// `--include`/`--exclude`/`--filter`/`--include-from`/`--exclude-from`/`-F` is
/// fed to the rule list at its argv position (exclude.c:parse_filter_str
/// appends in encounter order). `-C` is the exception: it only sets
/// `cvs_exclude`, and send_filter_list() appends its rules after all of them
/// (exclude.c:1951-1954).
///
/// # Errors
///
/// The first malformed rule or unreadable filter file, as the message upstream
/// prints before exiting.
pub fn build_filter_rules(
    inputs: FilterInputs,
    cvs: &filters::CvsSession,
    files: &dyn FilterFileSource,
) -> Result<Vec<FilterRuleSpec>, Message> {
    let mut filter_rules = Vec::new();
    let mut merge_stack = Vec::new();
    let mut cvs_exclude = false;
    let merge_base = files.base_dir();
    let from0 = inputs.from0;
    let mut delete_excluded_at = None;
    let mut ctx = ParseContext {
        base_dir: &merge_base,
        merge_stack: &mut merge_stack,
        cvs,
        files,
    };

    for token in inputs.order {
        let result = match token {
            // upstream: options.c:1625-1633 - `--include`/`--exclude` parse
            // with XFLG_OLD_PREFIXES, so `- pat`/`+ pat` flip the kind and `!`
            // clears the list.
            FilterOrderToken::Include(pattern) => parse_filter_str(
                &mut filter_rules,
                &pattern,
                argument(FilterRuleKind::Include),
                OLD_PREFIXES,
                &mut ctx,
            ),
            FilterOrderToken::Exclude(pattern) => parse_filter_str(
                &mut filter_rules,
                &pattern,
                argument(FilterRuleKind::Exclude),
                OLD_PREFIXES,
                &mut ctx,
            ),
            FilterOrderToken::IncludeFrom(file) => append_filter_rules_from_files(
                &mut filter_rules,
                std::slice::from_ref(&file),
                FilterRuleKind::Include,
                from0,
                files,
                Xflags::default(),
            ),
            FilterOrderToken::ExcludeFrom(file) => append_filter_rules_from_files(
                &mut filter_rules,
                std::slice::from_ref(&file),
                FilterRuleKind::Exclude,
                from0,
                files,
                Xflags::default(),
            ),
            // upstream: options.c:1620-1623 - rule_template(0), no xflags.
            FilterOrderToken::Filter(rule) => parse_filter_str(
                &mut filter_rules,
                &rule,
                argument(FilterRuleKind::Exclude),
                Xflags::default(),
                &mut ctx,
            ),
            FilterOrderToken::CvsExclude => {
                cvs_exclude = true;
                Ok(())
            }
            FilterOrderToken::AppleDoubleSkip => {
                append_apple_double_exclude_rules(&mut filter_rules)
            }
            FilterOrderToken::DeleteExcluded => {
                delete_excluded_at.get_or_insert(filter_rules.len());
                Ok(())
            }
        };
        result?;
    }
    if cvs_exclude {
        append_cvs_exclude_rules(&mut filter_rules, cvs);
    }

    // upstream: exclude.c:1478-1481 parse_rule_tok() - rules parsed before
    // --delete-excluded get no implicit FILTRULE_SENDER_SIDE.
    if let Some(at) = delete_excluded_at {
        filter_rules[..at]
            .iter_mut()
            .for_each(FilterRuleSpec::mark_parsed_before_delete_excluded);
    }

    Ok(filter_rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// upstream: exclude.c:1541-1551 parse_filter_str - `--exclude=!` pops the
    /// list while parsing, so no clear rule exists afterwards. A surviving one
    /// would be sent to the peer, and a pre-29 peer cannot carry it at all.
    #[test]
    fn old_prefix_clear_empties_the_list_instead_of_adding_a_rule() {
        let order = ["*.tmp", "!", "keep"]
            .into_iter()
            .map(|value| FilterOrderToken::Exclude(value.into()))
            .collect();
        let rules = build_filter_rules(
            FilterInputs {
                order,
                from0: false,
            },
            &filters::CvsSession::default(),
            &super::super::TestFiles,
        )
        .expect("rules parse");
        assert_eq!(rules, [FilterRuleSpec::exclude("keep")]);
    }
}
