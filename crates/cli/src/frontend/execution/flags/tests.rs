use super::debug::DebugFlagSettings;
use super::info::{INFO_FLAG_SPECS, InfoFlagSettings};
use super::output_words::{OutputWord, classify};
use super::*;
use crate::frontend::progress::{NameOutputLevel, ProgressSetting};
use std::ffi::OsString;

fn named(token: &str) -> (String, u8) {
    match classify(token) {
        OutputWord::Named { name, level } => (name.to_owned(), level),
        OutputWord::Help => ("<help>".to_owned(), 0),
        OutputWord::Every(level) => ("<all>".to_owned(), level),
    }
}

#[test]
fn output_word_without_a_suffix_is_level_one() {
    assert_eq!(named("progress"), ("progress".to_owned(), 1));
}

#[test]
fn output_word_takes_its_level_from_a_trailing_digit() {
    assert_eq!(named("progress2"), ("progress".to_owned(), 2));
}

// upstream: options.c parse_output_words has no negation prefix - `no<word>`
// and `-<word>` are ordinary (unknown) names and exit RERR_SYNTAX. The
// portable spelling for silencing a word is the `<word>0` suffix, which
// `output_word_level_zero_silences_a_word` pins as the surviving mechanism.
#[test]
fn a_no_prefixed_word_is_not_a_negation() {
    assert_eq!(named("noprogress"), ("noprogress".to_owned(), 1));
    let err = parse_info_flags(&[OsString::from("noprogress")])
        .expect_err("`noprogress` is an unknown info word, not a negation");
    assert!(
        err.to_string().contains("noprogress"),
        "error must name the rejected token: {err}"
    );
}

#[test]
fn a_dash_prefixed_word_is_not_a_negation() {
    assert_eq!(named("-progress"), ("-progress".to_owned(), 1));
    let _ = parse_info_flags(&[OsString::from("-progress")])
        .expect_err("`-progress` is an unknown info word, not a negation");
}

#[test]
fn output_word_level_zero_silences_a_word() {
    let settings = parse_info_flags(&[OsString::from("progress0")]).expect("progress0 parses");
    assert_eq!(settings.progress, ProgressSetting::Disabled);
}

// upstream: options.c parse_output_words skips the trailing-digit scan when
// the token starts with a digit (`if (!isDigit(str))`), so a bare integer is
// its own name and reaches the unknown-item error.
#[test]
fn a_bare_integer_is_a_word_name_not_a_level() {
    assert_eq!(named("2"), ("2".to_owned(), 1));
    let _ =
        parse_info_flags(&[OsString::from("2")]).expect_err("a bare integer is not an info word");
}

#[test]
fn info_flag_apply_help() {
    let mut settings = InfoFlagSettings::default();
    assert!(!settings.help_requested);
    settings.apply("help").unwrap();
    assert!(settings.help_requested);
}

#[test]
fn info_flag_apply_all() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all").unwrap();
    assert_eq!(settings.progress, ProgressSetting::PerFile);
    assert_eq!(settings.stats, Some(1));
}

#[test]
fn info_flag_apply_none() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all").unwrap();
    settings.apply("none").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Disabled);
    assert_eq!(settings.stats, Some(0));
}

#[test]
fn info_flag_apply_progress() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("progress").unwrap();
    assert_eq!(settings.progress, ProgressSetting::PerFile);
}

#[test]
fn info_flag_apply_progress2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("progress2").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Overall);
}

#[test]
fn info_flag_apply_invalid() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("invalid");
    assert!(result.is_err());
}

#[test]
fn parse_info_flags_empty_value_is_a_no_op() {
    let values = vec![OsString::from("")];
    parse_info_flags(&values).expect("upstream skips empty items rather than erroring");
}

#[test]
fn parse_info_flags_comma_separated() {
    let values = vec![OsString::from("progress,stats")];
    let result = parse_info_flags(&values).unwrap();
    assert_eq!(result.progress, ProgressSetting::PerFile);
    assert_eq!(result.stats, Some(1));
}

// upstream: options.c parse_output_words - the client-side parser rejects
// unknown info tokens so users see typos at their source.
#[test]
fn parse_info_flags_client_rejects_unknown_token() {
    let values = vec![OsString::from("future_unknown_flag")];
    let err = parse_info_flags(&values).expect_err("client mode must reject unknown tokens");
    assert!(
        err.text().contains("future_unknown_flag"),
        "error text should name the offending token: {}",
        err.text()
    );
}

// upstream: options.c parse_output_words - the `!am_server` guard means the
// server side silently accepts unknown tokens, preserving compatibility when
// a newer client forwards info names this build has not learned yet.
#[test]
fn parse_info_flags_server_accepts_unknown_token() {
    let values = vec![OsString::from("future_unknown_flag")];
    let settings =
        parse_info_flags_server(&values).expect("server mode must accept unknown tokens");
    assert_eq!(settings.progress, ProgressSetting::Unspecified);
    assert_eq!(settings.stats, None);
}

// Server-mode tolerance must still apply known tokens; only the unknown
// portion is skipped. Mirrors upstream's per-token loop in
// parse_output_words().
#[test]
fn parse_info_flags_server_mixes_known_and_unknown() {
    let values = vec![OsString::from("progress,future_unknown_flag,stats")];
    let settings = parse_info_flags_server(&values)
        .expect("server mode must accept unknown tokens alongside known ones");
    assert_eq!(settings.progress, ProgressSetting::PerFile);
    assert_eq!(settings.stats, Some(1));
}

#[test]
fn a_debug_word_shares_the_info_token_grammar() {
    assert_eq!(named("io"), ("io".to_owned(), 1));
    assert_eq!(named("io3"), ("io".to_owned(), 3));
    let _ = parse_debug_flags(&[OsString::from("noio")])
        .expect_err("`noio` is an unknown debug word, not a negation");
}

#[test]
fn debug_flag_apply_all() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all").unwrap();
    assert_eq!(settings.io, Some(1));
    assert_eq!(settings.flist, Some(1));
}

#[test]
fn debug_flag_apply_none() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all").unwrap();
    settings.apply("none").unwrap();
    assert_eq!(settings.io, Some(0));
    assert_eq!(settings.flist, Some(0));
}

#[test]
fn debug_flag_apply_io() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("io").unwrap();
    assert_eq!(settings.io, Some(1));
}

/// upstream: options.c:444-445 - levels beyond MAX_OUT_LEVEL (4) are clamped,
/// not rejected. `--debug=IO5` becomes IO level 4 in upstream.
#[test]
fn debug_flag_apply_io_level_clamped_to_max() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("io5").unwrap();
    assert_eq!(settings.io, Some(4));
}

#[test]
fn debug_flag_apply_invalid() {
    let mut settings = DebugFlagSettings::default();
    let result = settings.apply("invalid");
    assert!(result.is_err());
}

#[test]
fn parse_debug_flags_empty_value_is_a_no_op() {
    let values = vec![OsString::from("")];
    parse_debug_flags(&values).expect("upstream skips empty items rather than erroring");
}

#[test]
fn parse_debug_flags_help_requested() {
    let values = vec![OsString::from("help")];
    let result = parse_debug_flags(&values).unwrap();
    assert!(result.help_requested);
}

#[test]
fn parse_debug_flags_comma_separated() {
    let values = vec![OsString::from("io,flist")];
    let result = parse_debug_flags(&values).unwrap();
    assert_eq!(result.io, Some(1));
    assert_eq!(result.flist, Some(1));
}

#[test]
fn info_flag_progress0_disables() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("progress2").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Overall);
    settings.apply("progress0").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Disabled);
}

#[test]
fn info_flag_stats2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("stats2").unwrap();
    assert_eq!(settings.stats, Some(2));
}

#[test]
fn info_flag_stats3() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("stats3").unwrap();
    assert_eq!(settings.stats, Some(3));
}

#[test]
fn info_flag_name0() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("name0").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
}

#[test]
fn info_flag_name1() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("name").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedOnly));
}

#[test]
fn info_flag_name2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("name2").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
}

#[test]
fn info_flag_name_high_level_accepted() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("name5").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
}

#[test]
fn info_flag_flist0() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("flist0").unwrap();
    assert_eq!(settings.flist, Some(0));
}

#[test]
fn info_flag_flist2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("flist2").unwrap();
    assert_eq!(settings.flist, Some(2));
}

#[test]
fn info_flag_misc2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("misc2").unwrap();
    assert_eq!(settings.misc, Some(2));
}

#[test]
fn info_flag_skip2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("skip2").unwrap();
    assert_eq!(settings.skip, Some(2));
}

#[test]
fn info_flag_backup_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("backup").unwrap();
    assert_eq!(settings.backup, Some(1));
    // upstream clamps every explicit level to MAX_OUT_LEVEL (4).
    settings.apply("backup5").unwrap();
    assert_eq!(settings.backup, Some(4));
}

#[test]
fn info_flag_copy_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("copy").unwrap();
    assert_eq!(settings.copy, Some(1));
    settings.apply("copy3").unwrap();
    assert_eq!(settings.copy, Some(3));
}

#[test]
fn info_flag_del_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("del").unwrap();
    assert_eq!(settings.del, Some(1));
}

#[test]
fn info_flag_mount_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("mount").unwrap();
    assert_eq!(settings.mount, Some(1));
}

#[test]
fn info_flag_nonreg_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nonreg").unwrap();
    assert_eq!(settings.nonreg, Some(1));
}

#[test]
fn info_flag_remove_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("remove").unwrap();
    assert_eq!(settings.remove, Some(1));
}

#[test]
fn info_flag_symsafe_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("symsafe").unwrap();
    assert_eq!(settings.symsafe, Some(1));
}

#[test]
fn info_flag_numeric_1_enables_all() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all1").unwrap();
    assert_eq!(settings.progress, ProgressSetting::PerFile);
    assert_eq!(settings.stats, Some(1));
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedOnly));
    assert_eq!(settings.backup, Some(1));
    assert_eq!(settings.copy, Some(1));
    assert_eq!(settings.del, Some(1));
    assert_eq!(settings.flist, Some(1));
    assert_eq!(settings.misc, Some(1));
    assert_eq!(settings.mount, Some(1));
    assert_eq!(settings.nonreg, Some(1));
    assert_eq!(settings.remove, Some(1));
    assert_eq!(settings.skip, Some(1));
    assert_eq!(settings.symsafe, Some(1));
}

#[test]
fn info_flag_numeric_0_disables_all() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all").unwrap();
    settings.apply("all0").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Disabled);
    assert_eq!(settings.stats, Some(0));
    assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
    assert_eq!(settings.backup, Some(0));
    assert_eq!(settings.copy, Some(0));
    assert_eq!(settings.del, Some(0));
    assert_eq!(settings.flist, Some(0));
    assert_eq!(settings.misc, Some(0));
    assert_eq!(settings.mount, Some(0));
    assert_eq!(settings.nonreg, Some(0));
    assert_eq!(settings.remove, Some(0));
    assert_eq!(settings.skip, Some(0));
    assert_eq!(settings.symsafe, Some(0));
}

#[test]
fn info_flag_all_case_insensitive() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("ALL").unwrap();
    assert_eq!(settings.stats, Some(1));

    let mut settings = InfoFlagSettings::default();
    settings.apply("All").unwrap();
    assert_eq!(settings.stats, Some(1));
}

#[test]
fn info_flag_none_case_insensitive() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all").unwrap();
    settings.apply("NONE").unwrap();
    assert_eq!(settings.stats, Some(0));

    let mut settings = InfoFlagSettings::default();
    settings.apply("all").unwrap();
    settings.apply("None").unwrap();
    assert_eq!(settings.stats, Some(0));
}

#[test]
fn info_flag_help_case_insensitive() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("HELP").unwrap();
    assert!(settings.help_requested);
}

#[test]
fn parse_info_flags_multiple_values() {
    let values = vec![OsString::from("name"), OsString::from("stats2")];
    let result = parse_info_flags(&values).unwrap();
    assert_eq!(result.name, Some(NameOutputLevel::UpdatedOnly));
    assert_eq!(result.stats, Some(2));
}

#[test]
fn parse_info_flags_multiple_with_comma_separated() {
    let values = vec![OsString::from("name,copy"), OsString::from("stats2,del")];
    let result = parse_info_flags(&values).unwrap();
    assert_eq!(result.name, Some(NameOutputLevel::UpdatedOnly));
    assert_eq!(result.copy, Some(1));
    assert_eq!(result.stats, Some(2));
    assert_eq!(result.del, Some(1));
}

#[test]
fn parse_info_flags_later_overrides_earlier() {
    let values = vec![OsString::from("stats"), OsString::from("stats2")];
    let result = parse_info_flags(&values).unwrap();
    assert_eq!(result.stats, Some(2));
}

#[test]
fn parse_info_flags_all_then_override() {
    let values = vec![OsString::from("all"), OsString::from("progress0")];
    let result = parse_info_flags(&values).unwrap();
    assert_eq!(result.progress, ProgressSetting::Disabled);
    assert_eq!(result.stats, Some(1));
    assert_eq!(result.name, Some(NameOutputLevel::UpdatedOnly));
}

#[test]
fn parse_info_flags_none_then_enable() {
    let values = vec![OsString::from("none"), OsString::from("stats,name")];
    let result = parse_info_flags(&values).unwrap();
    assert_eq!(result.stats, Some(1));
    assert_eq!(result.name, Some(NameOutputLevel::UpdatedOnly));
    assert_eq!(result.progress, ProgressSetting::Disabled);
}

#[test]
fn parse_info_flags_help_terminates_early() {
    let values = vec![OsString::from("help")];
    let result = parse_info_flags(&values).unwrap();
    assert!(result.help_requested);
}

#[test]
fn info_flag_error_message_contains_flag_name() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("bogus");
    assert!(result.is_err());
    let msg = format!("{:?}", result.unwrap_err());
    assert!(msg.contains("bogus"), "error should mention the flag name");
}

// upstream: options.c:485-486 -
// `rprintf(FERROR, "Unknown %s item: \"%.*s\"\n", words[j].help, len, str)`.
// The sentinel's help field is the bare option name, so the diagnostic is
// exactly `Unknown --info item: "bogus"` with no trailing hint. oc previously
// invented `invalid --info flag 'bogus': use --info=help for supported flags`.
#[test]
fn info_flag_error_uses_the_upstream_wording() {
    let mut settings = InfoFlagSettings::default();
    let err = settings.apply("bogus").expect_err("bogus is unknown");
    assert_eq!(err.text(), "Unknown --info item: \"bogus\"");
    assert_eq!(err.code(), Some(1));
}

#[test]
fn info_flag_settings_default_is_unset() {
    let settings = InfoFlagSettings::default();
    assert_eq!(settings.progress, ProgressSetting::default());
    assert_eq!(settings.stats, None);
    assert_eq!(settings.name, None);
    assert_eq!(settings.backup, None);
    assert_eq!(settings.copy, None);
    assert_eq!(settings.del, None);
    assert_eq!(settings.flist, None);
    assert_eq!(settings.misc, None);
    assert_eq!(settings.mount, None);
    assert_eq!(settings.nonreg, None);
    assert_eq!(settings.remove, None);
    assert_eq!(settings.skip, None);
    assert_eq!(settings.symsafe, None);
    assert!(!settings.help_requested);
}

#[test]
fn info_flag_all_keywords_accepted() {
    let keywords = [
        "backup", "copy", "del", "flist", "misc", "mount", "name", "nonreg", "progress", "remove",
        "skip", "stats", "symsafe",
    ];
    for keyword in &keywords {
        let mut settings = InfoFlagSettings::default();
        let result = settings.apply(keyword);
        assert!(
            result.is_ok(),
            "keyword '{keyword}' should be accepted but got: {result:?}"
        );
    }
}

#[test]
fn debug_flag_all_keywords_accepted() {
    let keywords = [
        "acl", "backup", "bind", "chdir", "connect", "cmd", "del", "deltasum", "dup", "exit",
        "filter", "flist", "fuzzy", "genr", "hash", "hlink", "iconv", "io", "nstr", "own", "proto",
        "recv", "send", "time",
    ];
    for keyword in &keywords {
        let mut settings = DebugFlagSettings::default();
        let result = settings.apply(keyword);
        assert!(
            result.is_ok(),
            "debug keyword '{keyword}' should be accepted but got: {result:?}"
        );
    }
}

/// upstream: options.c:444-445 - all debug levels are clamped to MAX_OUT_LEVEL (4),
/// never rejected. Verify that levels at and beyond the documented per-flag maxima
/// are accepted and clamped.
#[test]
fn debug_flag_level_clamping() {
    // Within-range levels are stored as-is.
    let mut settings = DebugFlagSettings::default();
    settings.apply("backup2").unwrap();
    assert_eq!(settings.backup, Some(2));

    settings.apply("del3").unwrap();
    assert_eq!(settings.del, Some(3));

    settings.apply("deltasum4").unwrap();
    assert_eq!(settings.deltasum, Some(4));

    settings.apply("io4").unwrap();
    assert_eq!(settings.io, Some(4));

    // Beyond MAX_OUT_LEVEL: clamped to 4.
    let mut settings = DebugFlagSettings::default();
    settings.apply("backup5").unwrap();
    assert_eq!(settings.backup, Some(4));

    settings.apply("connect9").unwrap();
    assert_eq!(settings.connect, Some(4));

    settings.apply("cmd7").unwrap();
    assert_eq!(settings.cmd, Some(4));

    settings.apply("del8").unwrap();
    assert_eq!(settings.del, Some(4));

    settings.apply("deltasum5").unwrap();
    assert_eq!(settings.deltasum, Some(4));

    settings.apply("exit6").unwrap();
    assert_eq!(settings.exit, Some(4));

    settings.apply("filter5").unwrap();
    assert_eq!(settings.filter, Some(4));

    settings.apply("flist9").unwrap();
    assert_eq!(settings.flist, Some(4));

    settings.apply("fuzzy5").unwrap();
    assert_eq!(settings.fuzzy, Some(4));

    settings.apply("hlink5").unwrap();
    assert_eq!(settings.hlink, Some(4));

    settings.apply("iconv7").unwrap();
    assert_eq!(settings.iconv, Some(4));

    settings.apply("io5").unwrap();
    assert_eq!(settings.io, Some(4));

    settings.apply("own9").unwrap();
    assert_eq!(settings.own, Some(4));

    settings.apply("time6").unwrap();
    assert_eq!(settings.time, Some(4));
}

/// upstream: options.c:452-453 - "all" with a numeric suffix sets every flag to
/// min(suffix, MAX_OUT_LEVEL). e.g. `all4` sets all to 4, `all9` clamps to 4.
#[test]
fn debug_flag_all_with_level() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all4").unwrap();
    assert_eq!(settings.io, Some(4));
    assert_eq!(settings.flist, Some(4));
    assert_eq!(settings.hlink, Some(4));
    assert_eq!(settings.acl, Some(4));

    // Level beyond MAX_OUT_LEVEL is clamped.
    let mut settings = DebugFlagSettings::default();
    settings.apply("all9").unwrap();
    assert_eq!(settings.io, Some(4));
    assert_eq!(settings.hlink, Some(4));
}

#[test]
fn debug_flag_numeric_1_enables_all() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all1").unwrap();
    assert_eq!(settings.io, Some(1));
    assert_eq!(settings.proto, Some(1));
    assert_eq!(settings.flist, Some(1));
    assert_eq!(settings.acl, Some(1));
}

#[test]
fn debug_flag_numeric_0_disables_all() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all").unwrap();
    settings.apply("all0").unwrap();
    assert_eq!(settings.io, Some(0));
    assert_eq!(settings.proto, Some(0));
    assert_eq!(settings.flist, Some(0));
    assert_eq!(settings.acl, Some(0));
}

#[test]
fn debug_flag_iter_enabled_flags_returns_nonzero() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("io2").unwrap();
    settings.apply("flist").unwrap();
    settings.apply("del0").unwrap();

    let enabled: Vec<_> = settings.iter_enabled_flags().collect();
    assert!(enabled.contains(&("io", 2)));
    assert!(enabled.contains(&("flist", 1)));
    assert!(!enabled.iter().any(|(name, _)| *name == "del"));
}

#[test]
fn debug_flag_settings_default_is_unset() {
    let settings = DebugFlagSettings::default();
    assert_eq!(settings.acl, None);
    assert_eq!(settings.io, None);
    assert_eq!(settings.flist, None);
    assert!(!settings.help_requested);
}

#[test]
fn parse_debug_flags_multiple_values() {
    let values = vec![OsString::from("io"), OsString::from("flist2")];
    let result = parse_debug_flags(&values).unwrap();
    assert_eq!(result.io, Some(1));
    assert_eq!(result.flist, Some(2));
}

#[test]
fn parse_debug_flags_all_then_override() {
    let values = vec![OsString::from("all"), OsString::from("io0")];
    let result = parse_debug_flags(&values).unwrap();
    assert_eq!(result.io, Some(0));
    assert_eq!(result.flist, Some(1));
}

#[test]
fn info_help_text_lists_all_keywords() {
    let keywords = [
        "BACKUP", "COPY", "DEL", "FLIST", "MISC", "MOUNT", "NAME", "NONREG", "PROGRESS", "REMOVE",
        "SKIP", "STATS", "SYMSAFE",
    ];
    for keyword in &keywords {
        assert!(
            INFO_HELP_TEXT.contains(keyword),
            "INFO_HELP_TEXT should mention {keyword}"
        );
    }
}

// upstream: options.c output_item_help (rsync-3.4.1:489-495) renders the
// ALL/NONE pseudo-flags in uppercase using the same `"%-10s %s\n"` table
// row as the per-flag entries. The descriptive text inlines lowercase
// `all4` / `all0` examples; keep both shapes covered.
#[test]
fn info_help_text_mentions_all_and_none() {
    assert!(INFO_HELP_TEXT.contains("ALL"));
    assert!(INFO_HELP_TEXT.contains("NONE"));
    assert!(INFO_HELP_TEXT.contains("HELP"));
    assert!(INFO_HELP_TEXT.contains("(e.g. all4)"));
    assert!(INFO_HELP_TEXT.contains("(same as all0)"));
}

// upstream: options.c output_item_help (rsync-3.4.1:499-509) prints the
// per-verbosity summary block. info has three populated rows.
#[test]
fn info_help_text_lists_verbosity_summary() {
    assert!(INFO_HELP_TEXT.contains("Options added at each level of verbosity:"));
    assert!(INFO_HELP_TEXT.contains("0) NONREG"));
    assert!(INFO_HELP_TEXT.contains("1) COPY,DEL,FLIST,MISC,NAME,STATS,SYMSAFE"));
    assert!(INFO_HELP_TEXT.contains("2) BACKUP,MISC2,MOUNT,NAME2,REMOVE,SKIP"));
}

// upstream: options.c output_item_help (rsync-3.4.1:483) prints the
// "OPT or OPT1 ... OPT0 silences" preface verbatim.
#[test]
fn info_help_text_includes_opt_preface() {
    assert!(INFO_HELP_TEXT.starts_with(
        "Use OPT or OPT1 for level 1 output, OPT2 for level 2, etc.; OPT0 silences.\n"
    ));
}

// `no<flag>` / `-<flag>` are an internal-only extension not present in
// upstream rsync 3.4.1 (`options.c parse_output_words`); they must not be
// advertised in `--info=help` so users do not rely on a non-portable form.
#[test]
fn info_help_text_does_not_advertise_no_or_dash_prefix() {
    assert!(
        !INFO_HELP_TEXT.contains("noprogress"),
        "INFO_HELP_TEXT must not advertise the 'no<flag>' extension"
    );
    assert!(
        !INFO_HELP_TEXT.contains("'no'"),
        "INFO_HELP_TEXT must not advertise the 'no' prefix"
    );
    assert!(
        !INFO_HELP_TEXT.contains("'-'"),
        "INFO_HELP_TEXT must not advertise the '-' prefix"
    );
}

// Parser must still accept the internal-only `no<flag>` / `-<flag>` forms
// for backwards compatibility and server-mode token forwarding even though
// they are no longer advertised in `--info=help`.
#[test]
fn debug_help_text_lists_all_keywords() {
    let keywords = [
        "ACL", "BACKUP", "BIND", "CHDIR", "CONNECT", "CMD", "DEL", "DELTASUM", "DUP", "EXIT",
        "FILTER", "FLIST", "FUZZY", "GENR", "HASH", "HLINK", "ICONV", "IO", "NSTR", "OWN", "PROTO",
        "RECV", "SEND", "TIME",
    ];
    for keyword in &keywords {
        assert!(
            DEBUG_HELP_TEXT.contains(keyword),
            "DEBUG_HELP_TEXT should mention {keyword}"
        );
    }
}

// upstream: options.c output_item_help (rsync-3.4.1:489-495) renders the
// ALL/NONE pseudo-flags in uppercase and inlines lowercase `all4` / `all0`
// example tokens in the descriptive text.
#[test]
fn debug_help_text_mentions_all_and_none() {
    assert!(DEBUG_HELP_TEXT.contains("ALL"));
    assert!(DEBUG_HELP_TEXT.contains("NONE"));
    assert!(DEBUG_HELP_TEXT.contains("HELP"));
    assert!(DEBUG_HELP_TEXT.contains("(e.g. all4)"));
    assert!(DEBUG_HELP_TEXT.contains("(same as all0)"));
}

// upstream: options.c output_item_help (rsync-3.4.1:499-509) prints the
// per-verbosity summary block. debug_verbosity has levels 0 and 1 empty,
// so the summary lists levels 2-5 only (options.c:228-235).
#[test]
fn debug_help_text_lists_verbosity_summary() {
    assert!(DEBUG_HELP_TEXT.contains("Options added at each level of verbosity:"));
    assert!(DEBUG_HELP_TEXT.contains("2) BIND,CONNECT,CMD,DEL,DELTASUM,DUP,FILTER,FLIST,ICONV"));
    assert!(DEBUG_HELP_TEXT.contains(
        "3) ACL,BACKUP,CONNECT2,DEL2,DELTASUM2,EXIT,FILTER2,FLIST2,FUZZY,GENR,OWN,RECV,SEND,TIME"
    ));
    assert!(
        DEBUG_HELP_TEXT.contains("4) CMD2,DEL3,DELTASUM3,EXIT2,FLIST3,ICONV2,OWN2,PROTO,TIME2")
    );
    assert!(DEBUG_HELP_TEXT.contains("5) CHDIR,DELTASUM4,FLIST4,FUZZY2,HASH,HLINK"));
}

// upstream: options.c output_item_help (rsync-3.4.1:483) prints the
// "OPT or OPT1 ... OPT0 silences" preface verbatim.
#[test]
fn debug_help_text_includes_opt_preface() {
    assert!(DEBUG_HELP_TEXT.starts_with(
        "Use OPT or OPT1 for level 1 output, OPT2 for level 2, etc.; OPT0 silences.\n"
    ));
}

// upstream: options.c parse_output_words - "all<N>" sets every flag to
// level N (per-flag clamped). oc-rsync accepts a bare "<N>" token as a
// usability extension with the same semantics.
#[test]
fn info_flag_numeric_2_enables_all_at_level_2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all2").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Overall);
    assert_eq!(settings.stats, Some(2));
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
    assert_eq!(settings.flist, Some(2));
    assert_eq!(settings.misc, Some(2));
    assert_eq!(settings.skip, Some(2));
    // Flags with max level 1 stay clamped at 1.
    assert_eq!(settings.backup, Some(1));
    assert_eq!(settings.copy, Some(1));
    assert_eq!(settings.del, Some(1));
    assert_eq!(settings.mount, Some(1));
    assert_eq!(settings.nonreg, Some(1));
    assert_eq!(settings.remove, Some(1));
    assert_eq!(settings.symsafe, Some(1));
}

#[test]
fn info_flag_numeric_3_clamps_per_flag() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all3").unwrap();
    // STATS supports level 3.
    assert_eq!(settings.stats, Some(3));
    // PROGRESS, NAME, FLIST, MISC, SKIP clamp at 2.
    assert_eq!(settings.progress, ProgressSetting::Overall);
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
    assert_eq!(settings.flist, Some(2));
    assert_eq!(settings.misc, Some(2));
    assert_eq!(settings.skip, Some(2));
    // Boolean flags clamp at 1.
    assert_eq!(settings.copy, Some(1));
}

#[test]
fn info_flag_numeric_then_named_override() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all2").unwrap();
    settings.apply("name0").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
    // Other flags retained from the numeric pre-fill.
    assert_eq!(settings.stats, Some(2));
}

#[test]
fn parse_info_flags_numeric_then_named_in_one_arg() {
    let values = vec![OsString::from("all2,name0")];
    let result = parse_info_flags(&values).unwrap();
    assert_eq!(result.name, Some(NameOutputLevel::Disabled));
    assert_eq!(result.stats, Some(2));
    assert_eq!(result.flist, Some(2));
}

#[test]
fn info_flag_numeric_high_value_saturates() {
    // Out-of-range integers saturate at per-flag caps rather than erroring.
    let mut settings = InfoFlagSettings::default();
    settings.apply("all99").unwrap();
    assert_eq!(settings.stats, Some(3));
    assert_eq!(settings.flist, Some(2));
    assert_eq!(settings.copy, Some(1));
}

#[test]
fn info_flag_numeric_overflow_does_not_panic() {
    // A value that overflows u8 falls back to u8::MAX inside the parser,
    // which still saturates to per-flag caps.
    let mut settings = InfoFlagSettings::default();
    settings.apply("all999").unwrap();
    assert_eq!(settings.stats, Some(3));
}

#[test]
fn info_flag_spec_priority_matches_upstream_verbosity_groups() {
    // upstream: options.c info_verbosity[] (rsync-3.4.1:239-243).
    // NONREG sits in group 0 (always-on default); COPY/DEL/FLIST/MISC/NAME/
    // STATS/SYMSAFE/PROGRESS are in the level-1 group; BACKUP/MOUNT/REMOVE/
    // SKIP are in the level-2 group.
    let priority = |name: &str| {
        INFO_FLAG_SPECS
            .iter()
            .find(|spec| spec.name == name)
            .map(|spec| spec.priority)
    };
    assert_eq!(priority("nonreg"), Some(0));
    for name in [
        "copy", "del", "flist", "misc", "name", "stats", "symsafe", "progress",
    ] {
        assert_eq!(priority(name), Some(1), "{name} should be priority 1");
    }
    for name in ["backup", "mount", "remove", "skip"] {
        assert_eq!(priority(name), Some(2), "{name} should be priority 2");
    }
}

#[test]
fn info_flag_numeric_n_caps_each_priority_group_at_per_flag_max() {
    // `--info=2` enables every priority<=2 flag; per-flag caps still apply
    // (stats caps at 3, flist/misc/skip/name/progress at 2, others at 1).
    let mut settings = InfoFlagSettings::default();
    settings.apply("all2").unwrap();
    for spec in INFO_FLAG_SPECS {
        if spec.priority > 2 {
            continue;
        }
        let observed = match spec.name {
            "progress" => match settings.progress {
                ProgressSetting::Disabled | ProgressSetting::Unspecified => 0,
                ProgressSetting::PerFile => 1,
                ProgressSetting::Overall => 2,
            },
            "name" => match settings.name {
                Some(NameOutputLevel::Disabled) => 0,
                Some(NameOutputLevel::UpdatedOnly) => 1,
                Some(NameOutputLevel::UpdatedAndUnchanged) => 2,
                None => panic!("name unset"),
            },
            "stats" => settings.stats.unwrap(),
            "backup" => settings.backup.unwrap(),
            "copy" => settings.copy.unwrap(),
            "del" => settings.del.unwrap(),
            "flist" => settings.flist.unwrap(),
            "misc" => settings.misc.unwrap(),
            "mount" => settings.mount.unwrap(),
            "nonreg" => settings.nonreg.unwrap(),
            "remove" => settings.remove.unwrap(),
            "skip" => settings.skip.unwrap(),
            "symsafe" => settings.symsafe.unwrap(),
            other => panic!("unexpected spec {other}"),
        };
        assert_eq!(
            observed,
            2u8.min(spec.max_level),
            "{} cap at level 2",
            spec.name
        );
    }
}

// Tests for apply_to_thread_local - verifying that resolved InfoFlagSettings
// are correctly propagated to the thread-local VerbosityConfig used by
// info_log! callsites throughout the codebase.

#[test]
fn apply_to_thread_local_individual_flags() {
    logging::init(logging::VerbosityConfig::default());

    let mut settings = InfoFlagSettings::default();
    settings.apply("copy").unwrap();
    settings.apply("del").unwrap();
    settings.apply("flist2").unwrap();
    settings.apply_to_thread_local();

    assert!(logging::info_gte(logging::InfoFlag::Copy, 1));
    assert!(logging::info_gte(logging::InfoFlag::Del, 1));
    assert!(logging::info_gte(logging::InfoFlag::Flist, 2));
    assert!(!logging::info_gte(logging::InfoFlag::Flist, 3));
    // Unset flags should remain at their default (0)
    assert!(!logging::info_gte(logging::InfoFlag::Mount, 1));
}

#[test]
fn apply_to_thread_local_all_token() {
    logging::init(logging::VerbosityConfig::default());

    let mut settings = InfoFlagSettings::default();
    settings.apply("all").unwrap();
    settings.apply_to_thread_local();

    // All flags should be at level 1 (capped by max_level)
    assert!(logging::info_gte(logging::InfoFlag::Copy, 1));
    assert!(logging::info_gte(logging::InfoFlag::Del, 1));
    assert!(logging::info_gte(logging::InfoFlag::Flist, 1));
    assert!(logging::info_gte(logging::InfoFlag::Misc, 1));
    assert!(logging::info_gte(logging::InfoFlag::Name, 1));
    assert!(logging::info_gte(logging::InfoFlag::Stats, 1));
    assert!(logging::info_gte(logging::InfoFlag::Backup, 1));
    assert!(logging::info_gte(logging::InfoFlag::Mount, 1));
    assert!(logging::info_gte(logging::InfoFlag::Remove, 1));
    assert!(logging::info_gte(logging::InfoFlag::Skip, 1));
    assert!(logging::info_gte(logging::InfoFlag::Symsafe, 1));
    assert!(logging::info_gte(logging::InfoFlag::Nonreg, 1));
    assert!(logging::info_gte(logging::InfoFlag::Progress, 1));
}

#[test]
fn apply_to_thread_local_none_token() {
    // First enable everything via verbose level
    logging::init(logging::VerbosityConfig::from_verbose_level(2));
    assert!(logging::info_gte(logging::InfoFlag::Copy, 1));

    // Then apply none - should zero all flags
    let mut settings = InfoFlagSettings::default();
    settings.apply("none").unwrap();
    settings.apply_to_thread_local();

    assert!(!logging::info_gte(logging::InfoFlag::Copy, 1));
    assert!(!logging::info_gte(logging::InfoFlag::Del, 1));
    assert!(!logging::info_gte(logging::InfoFlag::Flist, 1));
    assert!(!logging::info_gte(logging::InfoFlag::Name, 1));
    assert!(!logging::info_gte(logging::InfoFlag::Stats, 1));
    assert!(!logging::info_gte(logging::InfoFlag::Progress, 1));
}

#[test]
fn apply_to_thread_local_numeric_level() {
    logging::init(logging::VerbosityConfig::default());

    let mut settings = InfoFlagSettings::default();
    settings.apply("all2").unwrap();
    settings.apply_to_thread_local();

    // Level 2 enables all flags, capped by per-flag max_level
    assert!(logging::info_gte(logging::InfoFlag::Stats, 2));
    assert!(!logging::info_gte(logging::InfoFlag::Stats, 3));
    assert!(logging::info_gte(logging::InfoFlag::Flist, 2));
    assert!(logging::info_gte(logging::InfoFlag::Name, 2));
    // Flags with max_level=1 are capped at 1
    assert!(logging::info_gte(logging::InfoFlag::Copy, 1));
    assert!(!logging::info_gte(logging::InfoFlag::Copy, 2));
}

#[test]
fn apply_to_thread_local_all_then_override() {
    logging::init(logging::VerbosityConfig::default());

    let flags = vec![OsString::from("all,name0")];
    let settings = parse_info_flags(&flags).unwrap();
    settings.apply_to_thread_local();

    // all sets name=1, then name0 overrides to 0
    assert!(!logging::info_gte(logging::InfoFlag::Name, 1));
    // Other flags should still be enabled
    assert!(logging::info_gte(logging::InfoFlag::Copy, 1));
    assert!(logging::info_gte(logging::InfoFlag::Stats, 1));
}

#[test]
fn apply_to_thread_local_verbose_then_info_override() {
    // Start with -v (verbose level 1) which sets NAME=1
    logging::init(logging::VerbosityConfig::from_verbose_level(1));
    assert!(logging::info_gte(logging::InfoFlag::Name, 1));
    assert!(!logging::info_gte(logging::InfoFlag::Backup, 1));

    // Apply --info=backup to enable backup without touching name
    let flags = vec![OsString::from("backup")];
    let settings = parse_info_flags(&flags).unwrap();
    settings.apply_to_thread_local();

    // Name should still be enabled from -v (not touched by --info=backup)
    assert!(logging::info_gte(logging::InfoFlag::Name, 1));
    // Backup should now be enabled
    assert!(logging::info_gte(logging::InfoFlag::Backup, 1));
}

#[test]
fn apply_to_thread_local_progress_levels() {
    logging::init(logging::VerbosityConfig::default());

    let mut settings = InfoFlagSettings::default();
    settings.apply("progress2").unwrap();
    settings.apply_to_thread_local();

    assert!(logging::info_gte(logging::InfoFlag::Progress, 2));

    // Reset and test progress disabled
    logging::init(logging::VerbosityConfig::from_verbose_level(1));
    let mut settings = InfoFlagSettings::default();
    settings.apply("progress0").unwrap();
    settings.apply_to_thread_local();

    assert!(!logging::info_gte(logging::InfoFlag::Progress, 1));
}

#[test]
fn apply_to_thread_local_unset_flags_not_touched() {
    // Start with verbose level 2 which sets many flags
    logging::init(logging::VerbosityConfig::from_verbose_level(2));
    assert!(logging::info_gte(logging::InfoFlag::Mount, 1));
    assert!(logging::info_gte(logging::InfoFlag::Copy, 1));

    // Apply only stats2 - should not touch other flags
    let flags = vec![OsString::from("stats2")];
    let settings = parse_info_flags(&flags).unwrap();
    settings.apply_to_thread_local();

    // Stats should be updated
    assert!(logging::info_gte(logging::InfoFlag::Stats, 2));
    // Other flags from verbose level 2 should remain untouched
    assert!(logging::info_gte(logging::InfoFlag::Mount, 1));
    assert!(logging::info_gte(logging::InfoFlag::Copy, 1));
}

// upstream: options.c parse_output_words matches a word by exact length
// against the table, so `no<word>` and `-<word>` are simply unknown names.
// Covering every word in the table rather than a sample keeps a future word
// from silently reintroducing the removed prefix.
#[test]
fn no_info_word_accepts_a_negation_prefix() {
    for spec in INFO_FLAG_SPECS {
        for token in [format!("no{}", spec.name), format!("-{}", spec.name)] {
            let _ = parse_info_flags(&[OsString::from(token.clone())]).expect_err(&format!(
                "`{token}` must be rejected, not read as a negation"
            ));
        }
    }
}

// Non-vacuity companion: the portable spelling upstream DOES accept must keep
// working for every word, or the test above would pass on a parser that
// rejects everything.
#[test]
fn every_info_word_accepts_the_level_zero_suffix() {
    for spec in INFO_FLAG_SPECS {
        let token = format!("{}0", spec.name);
        parse_info_flags(&[OsString::from(token.clone())])
            .unwrap_or_else(|e| panic!("`{token}` must parse: {e}"));
    }
}

#[test]
fn no_debug_word_accepts_a_negation_prefix() {
    for word in [
        "acl", "backup", "bind", "chdir", "connect", "cmd", "del", "deltasum", "dup", "exit",
        "filter", "flist", "fuzzy", "genr", "hash", "hlink", "iconv", "io", "nstr", "own", "proto",
        "recv", "send", "time", "iouring", "clone", "sockopt", "iocp",
    ] {
        for token in [format!("no{word}"), format!("-{word}")] {
            let _ = parse_debug_flags(&[OsString::from(token.clone())]).expect_err(&format!(
                "`{token}` must be rejected, not read as a negation"
            ));
        }
        let zero = format!("{word}0");
        parse_debug_flags(&[OsString::from(zero.clone())])
            .unwrap_or_else(|e| panic!("`{zero}` must parse: {e}"));
    }
}

#[test]
fn info_flag_progress3_is_clamped_not_rejected() {
    let mut settings = InfoFlagSettings::default();
    settings
        .apply("progress3")
        .expect("upstream clamps an out-of-range level, it never rejects one");
    assert_eq!(settings.progress, ProgressSetting::Overall);
}

#[test]
fn info_flag_stats4_is_clamped_not_rejected() {
    let mut settings = InfoFlagSettings::default();
    settings
        .apply("stats4")
        .expect("upstream clamps an out-of-range level, it never rejects one");
    assert_eq!(settings.stats, Some(4));
}

#[test]
fn info_flag_flist3_is_clamped_not_rejected() {
    let mut settings = InfoFlagSettings::default();
    settings
        .apply("flist3")
        .expect("upstream clamps an out-of-range level, it never rejects one");
    assert_eq!(settings.flist, Some(3));
}

#[test]
fn info_flag_misc3_is_clamped_not_rejected() {
    let mut settings = InfoFlagSettings::default();
    settings
        .apply("misc3")
        .expect("upstream clamps an out-of-range level, it never rejects one");
    assert_eq!(settings.misc, Some(3));
}

#[test]
fn info_flag_skip3_is_clamped_not_rejected() {
    let mut settings = InfoFlagSettings::default();
    settings
        .apply("skip3")
        .expect("upstream clamps an out-of-range level, it never rejects one");
    assert_eq!(settings.skip, Some(3));
}

// ---------------------------------------------------------------------------
// Unknown-item rejection parity (upstream options.c:443-490).
//
// MEASURED against target/interop/upstream-src/rsync-3.5.0/rsync. Each of the
// four pins below reproduces one cell where oc diverged from that binary; the
// `still` companions are the non-vacuity controls that stop "reject
// everything" from satisfying the pins.
// ---------------------------------------------------------------------------

// upstream: options.c:448-454 splits on ',' and skips only ZERO-LENGTH
// segments (`if (!len) continue;`); it never trims surrounding whitespace, and
// options.c:475 then compares the raw segment bytes with `strncasecmp`. So
// `--debug= flist` is the unknown item `" flist"` and exits RERR_SYNTAX.
// oc used to trim the token first and silently accept it.
#[test]
fn a_whitespace_padded_word_is_an_unknown_item_not_a_trimmed_one() {
    for token in [" flist", "flist ", "\tflist", " "] {
        let err = parse_debug_flags(&[OsString::from(token)]).expect_err(
            "upstream does not trim a --debug item, so a padded word is an unknown item",
        );
        assert_eq!(err.text(), format!("Unknown --debug item: \"{token}\""));
    }

    for token in [" progress", "progress ", "\tprogress", " "] {
        let err = parse_info_flags(&[OsString::from(token)]).expect_err(
            "upstream does not trim an --info item, so a padded word is an unknown item",
        );
        assert_eq!(err.text(), format!("Unknown --info item: \"{token}\""));
    }
}

// Non-vacuity companion: the same words WITHOUT the padding must still parse,
// so the pin above cannot be satisfied by rejecting every token.
#[test]
fn an_unpadded_word_is_still_accepted() {
    let settings = parse_debug_flags(&[OsString::from("flist")]).expect("`flist` is a debug word");
    assert_eq!(settings.flist, Some(1));

    let settings =
        parse_info_flags(&[OsString::from("progress")]).expect("`progress` is an info word");
    assert_eq!(settings.progress, ProgressSetting::PerFile);
}

// upstream: options.c:455-458 strips the trailing digits BEFORE the table
// lookup and options.c:485-486 then prints `"%.*s"` with that shortened `len`,
// so `--debug=BOGUS2` reports `BOGUS` - the level suffix is absent and the
// original case is preserved. oc used to echo the whole raw token.
#[test]
fn an_unknown_item_is_reported_without_its_level_suffix() {
    let err = parse_debug_flags(&[OsString::from("BOGUS2")]).expect_err("BOGUS is not a word");
    assert_eq!(err.text(), "Unknown --debug item: \"BOGUS\"");

    let err = parse_info_flags(&[OsString::from("bogus9")]).expect_err("bogus is not a word");
    assert_eq!(err.text(), "Unknown --info item: \"bogus\"");

    // A token that STARTS with a digit skips the strip entirely
    // (`if (!isDigit(str))`), so it is reported whole.
    let err = parse_debug_flags(&[OsString::from("2flist")]).expect_err("2flist is not a word");
    assert_eq!(err.text(), "Unknown --debug item: \"2flist\"");
}

// Non-vacuity companion: a KNOWN word carrying the same level suffix must
// still be accepted and must still select that level.
#[test]
fn a_known_word_with_a_level_suffix_is_still_accepted() {
    let settings = parse_debug_flags(&[OsString::from("FLIST2")]).expect("FLIST2 is a debug word");
    assert_eq!(settings.flist, Some(2));

    let settings = parse_info_flags(&[OsString::from("stats3")]).expect("stats3 is an info word");
    assert_eq!(settings.stats, Some(3));
}

// upstream: options.c:465-468 - `help` calls `output_item_help()` and then
// `exit_cleanup(0)` from inside the token loop, so nothing after it in the
// list (or in a later `--debug=` argument) is ever examined. oc used to
// validate the whole list first and so failed `--debug=help,bogus` with
// exit 1 where upstream prints the help and exits 0.
#[test]
fn a_help_item_short_circuits_the_rest_of_the_list() {
    let settings = parse_debug_flags(&[OsString::from("help,bogus")])
        .expect("`help` exits before `bogus` is examined");
    assert!(settings.help_requested);

    let settings = parse_debug_flags(&[OsString::from("help"), OsString::from("bogus")])
        .expect("`help` exits before a later --debug= argument is parsed");
    assert!(settings.help_requested);

    let settings = parse_info_flags(&[OsString::from("help,bogus")])
        .expect("`help` exits before `bogus` is examined");
    assert!(settings.help_requested);
}

// Non-vacuity companion: an unknown item BEFORE `help` still errors, so the
// short-circuit above cannot be satisfied by ignoring unknown items whenever
// `help` appears anywhere in the value.
#[test]
fn an_unknown_item_before_help_still_errors() {
    let err = parse_debug_flags(&[OsString::from("bogus,help")])
        .expect_err("`bogus` is reached before `help`");
    assert_eq!(err.text(), "Unknown --debug item: \"bogus\"");

    let err = parse_info_flags(&[OsString::from("bogus,help")])
        .expect_err("`bogus` is reached before `help`");
    assert_eq!(err.text(), "Unknown --info item: \"bogus\"");
}

// upstream: options.c:484-488 - the diagnostic is `Unknown <opt> item: "<tok>"`
// on FERROR (stderr) and the exit is `RERR_SYNTAX` = 1 (errcode.h:25). oc used
// to invent `invalid --debug flag '<tok>': use --debug=help for supported
// flags`.
#[test]
fn the_unknown_item_diagnostic_matches_upstream_wording() {
    let err = parse_debug_flags(&[OsString::from("bogus")]).expect_err("bogus is not a word");
    assert_eq!(err.text(), "Unknown --debug item: \"bogus\"");
    assert_eq!(err.code(), Some(1));

    let err = parse_info_flags(&[OsString::from("bogus")]).expect_err("bogus is not a word");
    assert_eq!(err.text(), "Unknown --info item: \"bogus\"");
    assert_eq!(err.code(), Some(1));
}

// Non-vacuity companion: the server-side decoder must stay permissive, because
// upstream's `!am_server` guard (options.c:484) skips the RERR_SYNTAX exit so a
// newer client's unknown word cannot kill the connection.
#[test]
fn the_server_side_decoder_still_ignores_an_unknown_item() {
    let settings = parse_info_flags_server(&[OsString::from("progress,bogus,stats")])
        .expect("the server side ignores an unknown item");
    assert_eq!(settings.progress, ProgressSetting::PerFile);
    assert_eq!(settings.stats, Some(1));
}
