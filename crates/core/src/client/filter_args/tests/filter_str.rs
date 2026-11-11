//! `parse_filter_str()`'s word-split walk and `add_rule()`'s `xflags` steps.

use super::super::filter_str::{Xflags, add_rule, rule_len};
use crate::client::{DirMergeOptions, FilterRuleSpec};

/// The flags every daemon module directive passes (clientserver.c:933-952).
const MODULE: Xflags = Xflags {
    old_prefixes: false,
    abs_if_slash: true,
    dir2wild3: true,
};

/// The rules a word-split `filter` string holds, one per word-split walk.
fn split(text: &str) -> Vec<&str> {
    let mut rules = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
        if rest.is_empty() {
            return rules;
        }
        let (rule, tail) = rest.split_at(rule_len(rest));
        rules.push(rule);
        rest = tail;
    }
}

/// What `add_rule` leaves of `rule` under `xflags`.
fn added(rule: FilterRuleSpec, xflags: Xflags) -> Option<FilterRuleSpec> {
    let mut list = Vec::new();
    add_rule(&mut list, rule, xflags);
    assert!(list.len() <= 1, "{list:?}");
    list.pop()
}

/// The pattern `add_rule` stores for `rule` under the module flags.
fn module_pattern(rule: FilterRuleSpec) -> String {
    added(rule, MODULE).expect("rule kept").pattern().to_owned()
}

#[test]
fn a_pattern_without_a_slash_is_added_as_written() {
    assert_eq!(module_pattern(FilterRuleSpec::exclude("*.tmp")), "*.tmp");
    assert_eq!(module_pattern(FilterRuleSpec::include("*.rs")), "*.rs");
}

/// upstream: exclude.c:297-306 - XFLG_ABS_IF_SLASH anchors a pattern holding
/// any slash, not just a leading one. A leading `/` is already the anchor.
#[test]
fn a_slash_anywhere_in_the_pattern_anchors_it() {
    assert_eq!(module_pattern(FilterRuleSpec::exclude("/etc")), "/etc");
    assert_eq!(
        module_pattern(FilterRuleSpec::exclude("subdir/file.txt")),
        "/subdir/file.txt"
    );
    assert_eq!(module_pattern(FilterRuleSpec::exclude("a/b/c")), "/a/b/c");
    assert_eq!(
        module_pattern(FilterRuleSpec::include("src/main.rs")),
        "/src/main.rs"
    );
}

/// A `**`-prefixed pattern holds a slash but stays unanchored: upstream sets
/// FILTRULE_WILD2_PREFIX on it (exclude.c:335-339), which already matches at
/// the root, and prepending `/` would turn `**/*.o` into `/**/*.o`, which no
/// longer matches a root-level `build.o`.
#[test]
fn a_leading_double_star_stays_unanchored() {
    assert_eq!(module_pattern(FilterRuleSpec::exclude("**/*.o")), "**/*.o");
}

/// upstream: exclude.c:287-314 - a trailing slash marks a directory rule and
/// is cut off before the slashes are counted, and XFLG_DIR2WILD3 then turns a
/// directory rule that is not an include into `dir/***`.
#[test]
fn a_trailing_slash_marks_a_directory_rule_and_is_not_counted() {
    assert_eq!(
        module_pattern(FilterRuleSpec::exclude("build/")),
        "build/***"
    );
    assert_eq!(
        module_pattern(FilterRuleSpec::protect("build/")),
        "build/***"
    );
    assert_eq!(module_pattern(FilterRuleSpec::exclude("a/b/")), "/a/b/***");
    assert_eq!(module_pattern(FilterRuleSpec::include("build/")), "build/");
    assert_eq!(module_pattern(FilterRuleSpec::risk("build/")), "build/");
}

/// Without XFLG_ABS_IF_SLASH and XFLG_DIR2WILD3 - every argv parse - the
/// pattern is added exactly as written.
#[test]
fn without_the_flags_nothing_is_rewritten() {
    for pattern in ["a/b", "build/", "/etc"] {
        let rule = added(FilterRuleSpec::exclude(pattern), Xflags::default());
        assert_eq!(rule.expect("rule kept").pattern(), pattern);
    }
}

/// upstream: exclude.c:279-285 - under XFLG_ABS_IF_SLASH a rule for the
/// sender alone is dropped as it is added; a receiver-side rule and one naming
/// both sides stay. Without the flag nothing is dropped.
#[test]
fn a_sender_side_rule_is_dropped_only_under_abs_if_slash() {
    assert!(added(FilterRuleSpec::hide("x"), MODULE).is_none());
    assert!(added(FilterRuleSpec::show("x"), MODULE).is_none());
    assert!(added(FilterRuleSpec::protect("x"), MODULE).is_some());
    assert!(added(FilterRuleSpec::risk("x"), MODULE).is_some());
    assert!(added(FilterRuleSpec::exclude("x").with_sides(true, true), MODULE).is_some());
    assert!(added(FilterRuleSpec::hide("x"), Xflags::default()).is_some());
}

/// upstream: exclude.c:297-298 - a merge rule's name is a path, so neither
/// rewrite reaches it.
#[test]
fn a_merge_file_name_is_never_rewritten() {
    let rule = FilterRuleSpec::dir_merge("sub/rules/", DirMergeOptions::default());
    assert_eq!(module_pattern(rule), "sub/rules/");
}

#[test]
fn split_single_exclude() {
    let tokens = split("- *.tmp");
    assert_eq!(tokens, vec!["- *.tmp"]);
}

#[test]
fn split_single_include() {
    let tokens = split("+ *.rs");
    assert_eq!(tokens, vec!["+ *.rs"]);
}

#[test]
fn split_multiple_rules() {
    let tokens = split("+ *.txt + *.rs + */ - *");
    assert_eq!(tokens, vec!["+ *.txt", "+ *.rs", "+ */", "- *"]);
}

#[test]
fn split_mixed_include_exclude() {
    let tokens = split("+ important.log + .keep.tmp - *.log - *.tmp");
    assert_eq!(
        tokens,
        vec!["+ important.log", "+ .keep.tmp", "- *.log", "- *.tmp"]
    );
}

#[test]
fn split_excludes_only() {
    let tokens = split("- *.tmp - *.bak - *.cache");
    assert_eq!(tokens, vec!["- *.tmp", "- *.bak", "- *.cache"]);
}

#[test]
fn split_empty() {
    let tokens = split("");
    assert!(tokens.is_empty());
}

#[test]
fn split_whitespace_only() {
    let tokens = split("   ");
    assert!(tokens.is_empty());
}

#[test]
fn split_keyword_rules() {
    let tokens = split("exclude *.tmp include *.rs");
    assert_eq!(tokens, vec!["exclude *.tmp", "include *.rs"]);
}

#[test]
fn split_bare_pattern() {
    let tokens = split("*.bak");
    assert_eq!(tokens, vec!["*.bak"]);
}

/// A one-character side prefix opens its own token.
///
/// upstream: the `default:` arm of `parse_rule_tok` (`exclude.c:1325-1329`)
/// makes `P` a rule character, and `parse_filter_str`'s word-split loop
/// hands it to that arm as its own token.
///
/// ⚠ NON-VACUITY: `- foo P bar` is the shape where the boundary is
/// OBSERVABLE. MEASURED against rsync 3.5.0 (module `foo`+`bar`+`keep`
/// +`ctl`): upstream serves `keep` and `ctl` only, hiding BOTH named files.
/// With the old `["+ ", "- ", "+/", "-/"]` table this line collapsed into
/// one rule whose pattern was the literal `foo P bar`, and oc served all
/// four - including `foo`, which even the `- foo` half names.
#[test]
fn split_short_side_prefix_opens_its_own_token() {
    assert_eq!(split("- foo P bar"), vec!["- foo", "P bar"], "P");
    assert_eq!(split("P bar H foo"), vec!["P bar", "H foo"], "H");
    assert_eq!(
        split("- foo R keep - k*"),
        vec!["- foo", "R keep", "- k*"],
        "R"
    );
    assert_eq!(split("S bar - b*"), vec!["S bar", "- b*"], "S");
}

/// An uppercase-initial bare word after a rule IS its own token.
///
/// ⚠ THIS EXPECTATION IS THE REVERSE OF WHAT IT WAS, and deliberately so.
/// It used to read `vec!["- foo Pictures"]`, which was the boundary
/// heuristic's answer rather than upstream's: the scanner asked "does
/// `Pictures` LOOK like a rule prefix?", decided no, and glued it onto the
/// pattern before it. Upstream never asks. `- ` takes one separator and a
/// pattern that ends at whitespace (`exclude.c:1444-1462`), so `Pictures`
/// starts a fresh rule - `P` plus the invalid modifier `i`, which
/// `exclude.c:1371-1380` refuses with `RERR_SYNTAX`.
///
/// MEASURED on loopback TCP, module holding
/// `foo bar baz keep ctl core Pictures README`: `filter = - foo Pictures` is
/// rc 5 upstream, was rc 0 in oc with EVERY FILE SERVED, and is rc 5 now.
#[test]
fn split_bare_word_in_rule_position_is_its_own_token() {
    assert_eq!(
        split("- foo Pictures"),
        vec!["- foo", "Pictures"],
        "Pictures"
    );
    assert_eq!(split("- foo README"), vec!["- foo", "README"], "README");
}

/// A keyword at END OF STRING terminates and opens its own token.
///
/// upstream: `exclude.c:1218-1227` `rule_strcmp` accepts the end of the
/// string as a terminator. The prefix table this replaced required a
/// trailing SPACE on every keyword, so a line-final `clear` matched
/// nothing, no boundary opened, and the whole line collapsed into ONE rule
/// whose pattern was the literal text `foo clear`.
#[test]
fn split_line_final_keyword_opens_its_own_token() {
    let tokens = split("- foo clear");
    assert_eq!(tokens, vec!["- foo", "clear"]);
}

/// `_` terminates a keyword exactly as whitespace does.
///
/// upstream: `exclude.c:1222` - `rule_strcmp` accepts `_` as a separator.
#[test]
fn split_underscore_terminated_keyword_opens_its_own_token() {
    let tokens = split("- foo hide_bar");
    assert_eq!(tokens, vec!["- foo", "hide_bar"]);
}

/// NON-VACUITY COMPANION for the two cells above: `hideout` is NOT a `hide`
/// rule, because `o` is not a separator - `rule_strcmp`
/// (`exclude.c:1218-1227`) returns NULL. Without this cell both would also
/// pass if the keyword test had been loosened to "any token STARTING WITH a
/// keyword", which is the obvious wrong fix.
///
/// ⚠ THIS EXPECTATION FLIPPED TOO, from `vec!["- foo hideout"]`. Failing
/// `rule_strcmp` does not make the word a pattern: upstream assigns
/// `s = RULE_STRCMP(...)` unconditionally, so a miss leaves `ch == 0` and
/// falls to `filter_rule_err("Unknown filter rule")` (`exclude.c:1363`).
/// The word is a REFUSED RULE, and the refusal is what the end-to-end cell
/// below pins. The old note here called that over-acceptance "a SEPARATE
/// defect tracked on its own" - this is that tracked defect, closed, and the
/// same measurement covers `mergeX` and every other unrecognised word.
#[test]
fn split_unterminated_keyword_is_its_own_token() {
    let tokens = split("- foo hideout");
    assert_eq!(tokens, vec!["- foo", "hideout"]);
}

/// THE POSITIONAL RULE: a `FILTRULE_WORD_SPLIT` pattern ends at the first
/// whitespace, and the next word is a fresh RULE.
///
/// upstream: `exclude.c:1457-1462` measures the pattern with
/// `while (!isspace(*cp) && *cp != '\0') cp++`, and `exclude.c:1485` resumes
/// the walk at `*pat_ptr + len`. Nothing looks ahead at the next word.
///
/// ⚠ NON-VACUITY: the heuristic returned ONE token for every row here.
/// MEASURED on loopback TCP against rsync 3.5.0 and oc daemons, module
/// holding `foo bar baz keep ctl core Pictures README`: `filter = - foo bar`
/// is rc 5 upstream and was rc 0 in oc WITH EVERY FILE SERVED - including
/// `foo`, which the rule's own first half names - because the glued pattern
/// `foo bar` matched nothing on disk.
#[test]
fn split_pattern_ends_at_the_first_whitespace() {
    assert_eq!(split("- foo bar"), vec!["- foo", "bar"]);
    assert_eq!(split("+ keep - bar baz"), vec!["+ keep", "- bar", "baz"]);
    // A TAB ends the pattern exactly as a space does.
    assert_eq!(split("- foo\tbar"), vec!["- foo", "bar"]);
}

/// A separator RUN is not one separator.
///
/// upstream consumes exactly ONE byte (`if (*s) s++`, `exclude.c:1444-1445`),
/// so a second separator lands where the pattern should start, the pattern
/// comes out EMPTY, and `exclude.c:1474-1476` refuses the rule. The word
/// after it is a rule of its own.
///
/// ⚠ THE RUN THAT MATTERS IS THE ONE BEFORE THE PATTERN. A run AFTER the
/// pattern (`- foo  bar`) is just the next lap's whitespace skip and proves
/// nothing about this decision: a mutant that consumed a whole run here
/// passed such a cell unharmed, which is why the rows below lead instead.
///
/// MEASURED, loopback TCP: `filter = -  foo` is rc 5 upstream and was rc 0
/// in oc WITH `foo` HIDDEN - oc built the operator's rule out of a config
/// upstream rejects outright. Same shape for `+  keep`, `P  bar` and
/// `exclude  bar`.
#[test]
fn split_consumes_exactly_one_separator() {
    // The prefix keeps ONE separator, the pattern is empty, so the token
    // ends at the second space and `foo` opens a rule of its own.
    assert_eq!(split("-  foo"), vec!["- ", "foo"]);
    assert_eq!(split("exclude  bar"), vec!["exclude ", "bar"]);
    // NON-VACUITY CONTROL: `_` is a separator too, and ONE of it still
    // yields a pattern - MEASURED rc 0 on both for `filter = -_foo`.
    assert_eq!(split("-_foo"), vec!["-_foo"]);
    // The after-the-pattern run, kept as the companion the note describes.
    assert_eq!(split("- foo  bar"), vec!["- foo", "bar"]);
}

/// The WORD_SPLIT STEP-BACK: a keyword separated by a TAB gets NO pattern.
///
/// upstream: `exclude.c:1367-1370` - inside the modifier scan a
/// `FILTRULE_WORD_SPLIT` whitespace that is not `' '` ends the run by moving
/// `s` BACK one byte, so the `if (*s) s++` that follows lands ON the tab and
/// the pattern length comes out zero. The token is the bare keyword, and the
/// word after it is a separate rule.
///
/// ⚠ THIS IS THE CELL ONLY A TRUE POSITIONAL PORT PASSES - a boundary
/// scanner has no step-back to reproduce, and no amount of prefix-table
/// tuning produces it. MEASURED: `filter = exclude<TAB>foo` is rc 5 upstream
/// (`unexpected end of filter rule`) where oc-base was rc 0 and served the
/// module with `foo` excluded - a DIFFERENT SERVED SET, not merely a
/// different exit code.
#[test]
fn split_tab_after_a_keyword_empties_the_pattern() {
    assert_eq!(split("exclude\tfoo"), vec!["exclude", "foo"]);
    // NON-VACUITY CONTROL: a single SPACE does not step back, so the very
    // same keyword keeps its pattern.
    assert_eq!(split("exclude foo"), vec!["exclude foo"]);
}
