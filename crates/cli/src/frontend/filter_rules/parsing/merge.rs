use std::ffi::OsString;

use core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use core::message::Message;

use super::super::directive::{FilterDirective, MergeDirective};
use super::helpers::{parse_modifier_run, split_short_modifiers, unexpected_end_of_filter_rule};
use super::rule_line::RuleLine;

/// Parses the modifier run that follows a `.`/`:`/`merge`/`dir-merge` prefix
/// into `DirMergeOptions`, plus whether `C` implied the `.cvsignore` name. The
/// modifier table is the one every rule shares.
///
/// upstream: exclude.c:1365-1443 parse_rule_tok() - which modifiers are legal
/// is decided by `filters::parse_rule_modifiers` under `FILTRULE_MERGE_FILE`;
/// this function only maps the resulting flags onto the merge options.
pub(super) fn parse_merge_modifiers(
    modifiers: &str,
    line: RuleLine<'_>,
) -> Result<(DirMergeOptions, bool), Message> {
    let mods = parse_modifier_run(modifiers, filters::ModifierScope::MERGE, line)?;
    Ok((merge_options(mods), mods.cvs_ignore))
}

/// Maps a merge rule's modifier flags onto the options its merged rules use.
///
/// `x` (`FILTRULE_XATTR`) is deliberately not mapped: FILTRULES_FROM_CONTAINER
/// (exclude.c:1229-1231) is ABS_PATH|INCLUDE|DIRECTORY|NEGATE|PERISHABLE, so
/// the merged rules never inherit it.
fn merge_options(mods: filters::RuleModifiers) -> DirMergeOptions {
    let mut options = DirMergeOptions::default();
    if mods.cvs_ignore {
        options = options.allow_list_clearing(true).cvs_mode(true);
    }
    if mods.word_split {
        options = options.use_whitespace().allow_comments(false);
    }
    if mods.no_inherit {
        options = options.inherit(false);
    }
    if mods.exclude_self {
        options = options.exclude_filter_file(true);
    }
    if mods.sender_side {
        options = options.sender_modifier();
    }
    if mods.receiver_side {
        options = options.receiver_modifier();
    }
    if mods.perishable {
        options = options.mark_perishable();
    }
    if mods.abs_path {
        options = options.anchor_root(true);
    }
    let enforced = mods.no_prefixes.then_some(if mods.no_prefixes_include {
        DirMergeEnforcedKind::Include
    } else {
        DirMergeEnforcedKind::Exclude
    });
    options.with_enforced_kind(enforced)
}

/// Parses a short merge directive: `. FILE` (merge) or `: FILE` (dir-merge),
/// including inline modifiers. Returns `None` when the text starts with neither
/// `.` nor `:`.
pub(super) fn parse_short_merge_directive(
    line: RuleLine<'_>,
) -> Option<Result<FilterDirective, Message>> {
    let text = line.text();
    let mut chars = text.chars();
    let first = chars.next()?;
    let is_dir_merge = match first {
        '.' => false,
        ':' => true,
        _ => return None,
    };

    let remainder = chars.as_str();
    // `remainder` starts one byte past the `.`/`:`, so that is the offset the
    // position in upstream's diagnostic is measured from.
    let (modifiers, rest) = split_short_modifiers(remainder);
    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, line) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    // `split_short_modifiers` already consumed the ONE separator that ends
    // the modifier run (upstream exclude.c:1444-1445, `if (*s) s++`), and the
    // merge FILENAME is then the rest of the rule verbatim (`len = strlen(s)`,
    // exclude.c:1465). `parse_merge_name` (exclude.c:696-753) only runs
    // `clean_fname` over it (:734), which collapses slashes and `..`, never
    // whitespace. So a trailing space is part of the merge file's NAME.
    //
    // MEASURED against rsync 3.5.0 over a source holding `a`, `b` and a filter
    // file literally named `.rsync-filter ` (trailing space) that reads `- b`,
    // with `--filter=': .rsync-filter '`: upstream finds the merge file and
    // copies only `a`.
    let pattern = rest;
    let pattern = if pattern.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else {
            // upstream: exclude.c:1475 - a merge/dir-merge with no file name is
            // `!len`, the same "unexpected end of filter rule" as a bare `-`.
            return Some(Err(unexpected_end_of_filter_rule(line)));
        }
    } else {
        pattern
    };

    if is_dir_merge {
        // upstream: exclude.c:354-357 add_rule takes the merge name after the
        // LAST '/', and setup_merge_file (exclude.c:799-801) rewrites
        // `ex->pattern` to that basename, so the per-directory open at
        // exclude.c:910 scans each directory for the basename. A path portion
        // only steers upstream's ancestor parent_dirscan; it is never joined
        // onto each scanned directory. Carrying `dir/.filt` through would make
        // every directory searched for `<dir>/dir/.filt` - a file that need not
        // exist, so the merge contributes no rules and whatever it was meant to
        // hide is served instead.
        //
        // Only the dir-merge (`:`) arm takes the basename. A plain merge (`.`)
        // names ONE file resolved once (exclude.c:696-753 parse_merge_name), so
        // its path portion is load-bearing and stays intact below.
        //
        // Same owner as the long `dir-merge` spelling in `directives.rs`, as the
        // daemon-side converter in `transfer/src/generator/filters.rs`, and as
        // the `:e` self-exclude.
        let name = filters::merge_file_basename(pattern);
        let rule = FilterRuleSpec::dir_merge(name.to_owned(), options);
        return Some(Ok(FilterDirective::Rule(rule)));
    }

    let enforced_kind = match options.enforced_kind() {
        Some(DirMergeEnforcedKind::Include) => Some(FilterRuleKind::Include),
        Some(DirMergeEnforcedKind::Exclude) => Some(FilterRuleKind::Exclude),
        None => None,
    };

    let directive =
        MergeDirective::new(OsString::from(pattern), enforced_kind).with_options(options);
    Some(Ok(FilterDirective::Merge(directive)))
}

#[cfg(test)]
mod tests {

    use filters::RuleSource;

    fn arg(text: &str) -> super::RuleLine<'_> {
        super::RuleLine::new(text, RuleSource::Argument)
    }
    use super::*;

    #[test]
    fn parse_merge_modifiers_empty() {
        let (options, assume_cvsignore) = parse_merge_modifiers("", arg("test")).unwrap();
        assert!(!assume_cvsignore);
        assert_eq!(options.enforced_kind(), None);
    }

    #[test]
    fn parse_merge_modifiers_exclude() {
        let (options, _) = parse_merge_modifiers("-", arg(":- file")).unwrap();
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    }

    #[test]
    fn parse_merge_modifiers_include() {
        let (options, _) = parse_merge_modifiers("+", arg(":+ file")).unwrap();
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    }

    #[test]
    fn parse_merge_modifiers_conflicting_plus_minus() {
        let result = parse_merge_modifiers("+-", arg(":+- file"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_conflicting_minus_plus() {
        let result = parse_merge_modifiers("-+", arg(":-+ file"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_cvsignore() {
        let (options, assume_cvsignore) = parse_merge_modifiers("C", arg(":C")).unwrap();
        assert!(assume_cvsignore);
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
        assert!(options.uses_whitespace());
        assert!(!options.allows_comments());
        // upstream: exclude.c:1402-1409 - `C` modifier sets FILTRULE_CVS_IGNORE.
        // We record this so the wire encoder can forward it as cvs_exclude=true.
        assert!(options.is_cvs_mode());
    }

    #[test]
    fn parse_merge_modifiers_cvsignore_with_include_error() {
        let result = parse_merge_modifiers("+C", arg(":+C file"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_exclude_self_dir_merge() {
        let (options, _) = parse_merge_modifiers("e", arg(":e file")).unwrap();
        assert!(options.excludes_self());
    }

    #[test]
    fn parse_merge_modifiers_exclude_self_plain_merge() {
        // upstream exclude.c:1410-1414 guards `e` only by FILTRULE_MERGE_FILE,
        // which a plain merge (`.`) sets too, so `.e FILE` is valid.
        let (options, _) = parse_merge_modifiers("e", arg(".e file")).unwrap();
        assert!(options.excludes_self());
    }

    #[test]
    fn parse_merge_modifiers_no_inherit_dir_merge() {
        let (options, _) = parse_merge_modifiers("n", arg(":n file")).unwrap();
        assert!(!options.inherit_rules());
    }

    #[test]
    fn parse_merge_modifiers_no_inherit_plain_merge() {
        // upstream exclude.c:1415-1419 guards `n` the same way as `e`.
        let (options, _) = parse_merge_modifiers("n", arg(".n file")).unwrap();
        assert!(!options.inherit_rules());
    }

    #[test]
    fn parse_short_plain_merge_accepts_exclude_self_modifier() {
        // `.e FILE`: `e` is a modifier, not part of the file name.
        let directive = parse_short_merge_directive(arg(".e rules"))
            .expect("recognized")
            .expect("parses");
        match directive {
            FilterDirective::Merge(merge) => {
                assert_eq!(merge.source(), std::ffi::OsStr::new("rules"));
                assert!(merge.options().excludes_self());
            }
            other => panic!("expected a merge directive, got {other:?}"),
        }
    }

    #[test]
    fn parse_merge_modifiers_whitespace() {
        let (options, _) = parse_merge_modifiers("w", arg(":w file")).unwrap();
        assert!(options.uses_whitespace());
        assert!(!options.allows_comments());
    }

    #[test]
    fn parse_merge_modifiers_sender() {
        let (options, _) = parse_merge_modifiers("s", arg(":s file")).unwrap();
        assert_eq!(options.sender_side_override(), Some(true));
    }

    #[test]
    fn parse_merge_modifiers_receiver() {
        let (options, _) = parse_merge_modifiers("r", arg(":r file")).unwrap();
        assert_eq!(options.receiver_side_override(), Some(true));
    }

    #[test]
    fn parse_merge_modifiers_perishable() {
        let (options, _) = parse_merge_modifiers("p", arg(":p file")).unwrap();
        assert!(options.perishable());
    }

    #[test]
    fn parse_merge_modifiers_anchor_root() {
        let (options, _) = parse_merge_modifiers("/", arg(":/ file")).unwrap();
        assert!(options.anchor_root_enabled());
    }

    #[test]
    fn parse_merge_modifiers_unknown() {
        // `z` is outside upstream's modifier set `- + / ! C e n p r s w x`
        // (exclude.c:1381-1441); `x` is in it and must parse.
        let result = parse_merge_modifiers("z", arg(":z file"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_combined() {
        let (options, _) = parse_merge_modifiers("-sp", arg(":- file")).unwrap();
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
        assert_eq!(options.sender_side_override(), Some(true));
        assert!(options.perishable());
    }

    #[test]
    fn parse_short_merge_directive_dot() {
        let result = parse_short_merge_directive(arg(". filter.txt"));
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        assert!(matches!(directive, FilterDirective::Merge(_)));
    }

    #[test]
    fn parse_short_merge_directive_colon() {
        let result = parse_short_merge_directive(arg(": .rsync-filter"));
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        assert!(matches!(directive, FilterDirective::Rule(_)));
    }

    #[test]
    fn parse_short_merge_directive_cvsignore() {
        let result = parse_short_merge_directive(arg(":C"));
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        // CVS ignore implies .cvsignore pattern
        assert!(matches!(directive, FilterDirective::Rule(_)));
    }

    #[test]
    fn parse_short_merge_directive_not_merge() {
        let result = parse_short_merge_directive(arg("+ include"));
        assert!(result.is_none());
    }

    #[test]
    fn parse_short_merge_directive_exclude_modifier() {
        let result = parse_short_merge_directive(arg(":- filter"));
        assert!(result.is_some());
        if let Some(Ok(FilterDirective::Rule(spec))) = result {
            // The rule should have exclude enforced
            let _ = spec;
        }
    }

    #[test]
    fn parse_short_merge_directive_include_modifier() {
        let result = parse_short_merge_directive(arg(":+ filter"));
        assert!(result.is_some());
    }

    /// `:  ` is NOT a missing file name: one separator is consumed
    /// (exclude.c:1444-1445) and the remaining space is the name
    /// (exclude.c:1465). MEASURED against rsync 3.5.0: `--filter=':  '` exits 0
    /// and transfers a source file literally named `  `, so the dir-merge name
    /// it registered was ` `.
    #[test]
    fn parse_short_merge_directive_keeps_a_space_only_name() {
        let result = parse_short_merge_directive(arg(":  "))
            .expect("`:  ` is a dir-merge directive")
            .expect("a space is a legal file name");
        match result {
            FilterDirective::Rule(rule) => assert_eq!(rule.pattern(), " "),
            other => panic!("expected a dir-merge rule, got {other:?}"),
        }
    }

    /// The `.` sibling of the cell above. MEASURED against rsync 3.5.0:
    /// `--filter='.  '` reports `failed to open exclude file  ` and exits 11,
    /// naming the file ` ` - it is a real path, not a parse error.
    #[test]
    fn parse_short_merge_directive_dot_keeps_a_space_only_path() {
        let result = parse_short_merge_directive(arg(".  "))
            .expect("`.  ` is a merge directive")
            .expect("a space is a legal file path");
        match result {
            FilterDirective::Merge(directive) => {
                assert_eq!(directive.source(), OsString::from(" "));
            }
            other => panic!("expected a merge directive, got {other:?}"),
        }
    }

    /// A dir-merge name is a NAME, so a path portion is dropped: upstream's
    /// `setup_merge_file` (exclude.c:799-801) rewrites `ex->pattern` to the
    /// basename `add_rule` took (exclude.c:354-357), and the per-directory open
    /// at exclude.c:910 reads the rewritten value.
    ///
    /// MEASURED against rsync 3.5.0 with `-r --filter=': dir/.filt'` over
    /// `src/.filt` plus `src/sub/{.filt,bait,keep}` where `src/sub/.filt` reads
    /// `- bait`: upstream hides `bait`.
    #[test]
    fn parse_short_dir_merge_takes_the_basename_after_the_last_slash() {
        for (given, want) in [
            ("dir/.filt", ".filt"),
            ("a/b/c/.rsync-filter", ".rsync-filter"),
            ("/.rsync-filter", ".rsync-filter"),
            (".rsync-filter", ".rsync-filter"),
        ] {
            let result = parse_short_merge_directive(arg(&format!(": {given}")))
                .expect("`:` is a dir-merge directive")
                .expect("directive parses");
            match result {
                FilterDirective::Rule(rule) => {
                    assert_eq!(rule.pattern(), want, "merge name for {given:?}");
                }
                other => panic!("expected a dir-merge rule for {given:?}, got {other:?}"),
            }
        }
    }

    /// The control that keeps the cell above honest, and the reason the basename
    /// rule is NOT applied to both arms: a plain `merge` names ONE file resolved
    /// once (exclude.c:696-753 `parse_merge_name`), never re-opened per
    /// directory, so its path portion is load-bearing. Taking the basename here
    /// would read a different file.
    #[test]
    fn parse_short_plain_merge_keeps_the_whole_path() {
        let result = parse_short_merge_directive(arg(". dir/.filt"))
            .expect("`.` is a merge directive")
            .expect("directive parses");
        match result {
            FilterDirective::Merge(directive) => {
                assert_eq!(directive.source(), OsString::from("dir/.filt"));
            }
            other => panic!("expected a merge directive, got {other:?}"),
        }
    }

    /// Non-vacuity control for the two cells above: a directive with NO
    /// remainder at all is still the missing-name error, so those cells cannot
    /// pass merely because the parser stopped erroring.
    #[test]
    fn parse_short_merge_directive_still_errors_with_no_name() {
        assert!(
            parse_short_merge_directive(arg(":"))
                .expect("`:` is a dir-merge directive")
                .is_err()
        );
        assert!(
            parse_short_merge_directive(arg("."))
                .expect("`.` is a merge directive")
                .is_err()
        );
    }

    #[test]
    fn parse_short_merge_directive_with_modifiers() {
        let result = parse_short_merge_directive(arg(":en .filter"));
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        assert!(matches!(directive, FilterDirective::Rule(_)));
    }
}
