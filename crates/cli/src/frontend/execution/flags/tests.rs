use super::debug::DebugFlagSettings;
use super::info::{INFO_FLAG_SPECS, InfoFlagSettings};
use super::*;
use crate::frontend::progress::{NameOutputLevel, ProgressSetting};
use std::ffi::OsString;

#[test]
fn info_flag_parse_flag_and_level_default() {
    let settings = InfoFlagSettings::default();
    let (name, level) = settings.parse_flag_and_level("progress");
    assert_eq!(name, "progress");
    assert_eq!(level, 1);
}

#[test]
fn info_flag_parse_flag_and_level_with_number() {
    let settings = InfoFlagSettings::default();
    let (name, level) = settings.parse_flag_and_level("progress2");
    assert_eq!(name, "progress");
    assert_eq!(level, 2);
}

#[test]
fn info_flag_parse_flag_and_level_no_prefix() {
    let settings = InfoFlagSettings::default();
    let (name, level) = settings.parse_flag_and_level("noprogress");
    assert_eq!(name, "progress");
    assert_eq!(level, 0);
}

#[test]
fn info_flag_parse_flag_and_level_dash_prefix() {
    let settings = InfoFlagSettings::default();
    let (name, level) = settings.parse_flag_and_level("-progress");
    assert_eq!(name, "progress");
    assert_eq!(level, 0);
}

#[test]
fn info_flag_apply_help() {
    let mut settings = InfoFlagSettings::default();
    assert!(!settings.help_requested);
    settings.apply("help", "help").unwrap();
    assert!(settings.help_requested);
}

#[test]
fn info_flag_apply_all() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all", "all").unwrap();
    assert_eq!(settings.progress, ProgressSetting::PerFile);
    assert_eq!(settings.stats, Some(1));
}

#[test]
fn info_flag_apply_none() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all", "all").unwrap();
    settings.apply("none", "none").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Disabled);
    assert_eq!(settings.stats, Some(0));
}

#[test]
fn info_flag_apply_progress() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("progress", "progress").unwrap();
    assert_eq!(settings.progress, ProgressSetting::PerFile);
}

#[test]
fn info_flag_apply_progress2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("progress2", "progress2").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Overall);
}

#[test]
fn info_flag_apply_invalid() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("invalid", "invalid");
    assert!(result.is_err());
}

#[test]
fn parse_info_flags_empty_value() {
    let values = vec![OsString::from("")];
    let result = parse_info_flags(&values);
    assert!(result.is_err());
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
fn debug_flag_parse_flag_and_level_default() {
    let settings = DebugFlagSettings::default();
    let (name, level) = settings.parse_flag_and_level("io");
    assert_eq!(name, "io");
    assert_eq!(level, 1);
}

#[test]
fn debug_flag_parse_flag_and_level_with_number() {
    let settings = DebugFlagSettings::default();
    let (name, level) = settings.parse_flag_and_level("io3");
    assert_eq!(name, "io");
    assert_eq!(level, 3);
}

#[test]
fn debug_flag_apply_all() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all", "all").unwrap();
    assert_eq!(settings.io, Some(1));
    assert_eq!(settings.flist, Some(1));
}

#[test]
fn debug_flag_apply_none() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all", "all").unwrap();
    settings.apply("none", "none").unwrap();
    assert_eq!(settings.io, Some(0));
    assert_eq!(settings.flist, Some(0));
}

#[test]
fn debug_flag_apply_io() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("io", "io").unwrap();
    assert_eq!(settings.io, Some(1));
}

/// upstream: options.c:454-455 - levels beyond MAX_OUT_LEVEL (4) are clamped,
/// not rejected. `--debug=IO5` becomes IO level 4 in upstream.
#[test]
fn debug_flag_apply_io_level_clamped_to_max() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("io5", "io5").unwrap();
    assert_eq!(settings.io, Some(4));
}

#[test]
fn debug_flag_apply_invalid() {
    let mut settings = DebugFlagSettings::default();
    let result = settings.apply("invalid", "invalid");
    assert!(result.is_err());
}

#[test]
fn parse_debug_flags_empty_value() {
    let values = vec![OsString::from("")];
    let result = parse_debug_flags(&values);
    assert!(result.is_err());
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
    settings.apply("progress2", "progress2").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Overall);
    settings.apply("progress0", "progress0").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Disabled);
}

#[test]
fn info_flag_progress3_rejected() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("progress3", "progress3");
    assert!(result.is_err());
}

#[test]
fn info_flag_stats2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("stats2", "stats2").unwrap();
    assert_eq!(settings.stats, Some(2));
}

#[test]
fn info_flag_stats3() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("stats3", "stats3").unwrap();
    assert_eq!(settings.stats, Some(3));
}

#[test]
fn info_flag_stats4_rejected() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("stats4", "stats4");
    assert!(result.is_err());
}

#[test]
fn info_flag_name0() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("name0", "name0").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
}

#[test]
fn info_flag_name1() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("name", "name").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedOnly));
}

#[test]
fn info_flag_name2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("name2", "name2").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
}

#[test]
fn info_flag_name_high_level_accepted() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("name5", "name5").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
}

#[test]
fn info_flag_flist0() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("flist0", "flist0").unwrap();
    assert_eq!(settings.flist, Some(0));
}

#[test]
fn info_flag_flist2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("flist2", "flist2").unwrap();
    assert_eq!(settings.flist, Some(2));
}

#[test]
fn info_flag_flist3_rejected() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("flist3", "flist3");
    assert!(result.is_err());
}

#[test]
fn info_flag_misc2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("misc2", "misc2").unwrap();
    assert_eq!(settings.misc, Some(2));
}

#[test]
fn info_flag_misc3_rejected() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("misc3", "misc3");
    assert!(result.is_err());
}

#[test]
fn info_flag_skip2() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("skip2", "skip2").unwrap();
    assert_eq!(settings.skip, Some(2));
}

#[test]
fn info_flag_skip3_rejected() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("skip3", "skip3");
    assert!(result.is_err());
}

#[test]
fn info_flag_backup_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("backup", "backup").unwrap();
    assert_eq!(settings.backup, Some(1));
    settings.apply("backup5", "backup5").unwrap();
    assert_eq!(settings.backup, Some(5));
}

#[test]
fn info_flag_copy_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("copy", "copy").unwrap();
    assert_eq!(settings.copy, Some(1));
    settings.apply("copy3", "copy3").unwrap();
    assert_eq!(settings.copy, Some(3));
}

#[test]
fn info_flag_del_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("del", "del").unwrap();
    assert_eq!(settings.del, Some(1));
}

#[test]
fn info_flag_mount_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("mount", "mount").unwrap();
    assert_eq!(settings.mount, Some(1));
}

#[test]
fn info_flag_nonreg_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nonreg", "nonreg").unwrap();
    assert_eq!(settings.nonreg, Some(1));
}

#[test]
fn info_flag_remove_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("remove", "remove").unwrap();
    assert_eq!(settings.remove, Some(1));
}

#[test]
fn info_flag_symsafe_any_level() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("symsafe", "symsafe").unwrap();
    assert_eq!(settings.symsafe, Some(1));
}

#[test]
fn info_flag_no_prefix_copy() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nocopy", "nocopy").unwrap();
    assert_eq!(settings.copy, Some(0));
}

#[test]
fn info_flag_dash_prefix_copy() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("-copy", "-copy").unwrap();
    assert_eq!(settings.copy, Some(0));
}

#[test]
fn info_flag_no_prefix_del() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nodel", "nodel").unwrap();
    assert_eq!(settings.del, Some(0));
}

#[test]
fn info_flag_no_prefix_stats() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nostats", "nostats").unwrap();
    assert_eq!(settings.stats, Some(0));
}

#[test]
fn info_flag_no_prefix_name() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("noname", "noname").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
}

#[test]
fn info_flag_no_prefix_skip() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("noskip", "noskip").unwrap();
    assert_eq!(settings.skip, Some(0));
}

#[test]
fn info_flag_no_prefix_flist() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("noflist", "noflist").unwrap();
    assert_eq!(settings.flist, Some(0));
}

#[test]
fn info_flag_no_prefix_misc() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nomisc", "nomisc").unwrap();
    assert_eq!(settings.misc, Some(0));
}

#[test]
fn info_flag_no_prefix_backup() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nobackup", "nobackup").unwrap();
    assert_eq!(settings.backup, Some(0));
}

#[test]
fn info_flag_no_prefix_mount() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nomount", "nomount").unwrap();
    assert_eq!(settings.mount, Some(0));
}

#[test]
fn info_flag_no_prefix_nonreg() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nononreg", "nononreg").unwrap();
    assert_eq!(settings.nonreg, Some(0));
}

#[test]
fn info_flag_no_prefix_remove() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("noremove", "noremove").unwrap();
    assert_eq!(settings.remove, Some(0));
}

#[test]
fn info_flag_no_prefix_symsafe() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("nosymsafe", "nosymsafe").unwrap();
    assert_eq!(settings.symsafe, Some(0));
}

#[test]
fn info_flag_numeric_1_enables_all() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("1", "1").unwrap();
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
    settings.apply("all", "all").unwrap();
    settings.apply("0", "0").unwrap();
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
    settings.apply("ALL", "ALL").unwrap();
    assert_eq!(settings.stats, Some(1));

    let mut settings = InfoFlagSettings::default();
    settings.apply("All", "All").unwrap();
    assert_eq!(settings.stats, Some(1));
}

#[test]
fn info_flag_none_case_insensitive() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("all", "all").unwrap();
    settings.apply("NONE", "NONE").unwrap();
    assert_eq!(settings.stats, Some(0));

    let mut settings = InfoFlagSettings::default();
    settings.apply("all", "all").unwrap();
    settings.apply("None", "None").unwrap();
    assert_eq!(settings.stats, Some(0));
}

#[test]
fn info_flag_help_case_insensitive() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("HELP", "HELP").unwrap();
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
    let result = settings.apply("bogus", "bogus");
    assert!(result.is_err());
    let msg = format!("{:?}", result.unwrap_err());
    assert!(msg.contains("bogus"), "error should mention the flag name");
}

#[test]
fn info_flag_error_suggests_help() {
    let mut settings = InfoFlagSettings::default();
    let result = settings.apply("bogus", "bogus");
    assert!(result.is_err());
    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("--info=help"),
        "error should suggest --info=help"
    );
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
        let result = settings.apply(keyword, keyword);
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
        let result = settings.apply(keyword, keyword);
        assert!(
            result.is_ok(),
            "debug keyword '{keyword}' should be accepted but got: {result:?}"
        );
    }
}

/// upstream: options.c:454-455 - all debug levels are clamped to MAX_OUT_LEVEL (4),
/// never rejected. Verify that levels at and beyond the documented per-flag maxima
/// are accepted and clamped.
#[test]
fn debug_flag_level_clamping() {
    // Within-range levels are stored as-is.
    let mut settings = DebugFlagSettings::default();
    settings.apply("backup2", "backup2").unwrap();
    assert_eq!(settings.backup, Some(2));

    settings.apply("del3", "del3").unwrap();
    assert_eq!(settings.del, Some(3));

    settings.apply("deltasum4", "deltasum4").unwrap();
    assert_eq!(settings.deltasum, Some(4));

    settings.apply("io4", "io4").unwrap();
    assert_eq!(settings.io, Some(4));

    // Beyond MAX_OUT_LEVEL: clamped to 4.
    let mut settings = DebugFlagSettings::default();
    settings.apply("backup5", "backup5").unwrap();
    assert_eq!(settings.backup, Some(4));

    settings.apply("connect9", "connect9").unwrap();
    assert_eq!(settings.connect, Some(4));

    settings.apply("cmd7", "cmd7").unwrap();
    assert_eq!(settings.cmd, Some(4));

    settings.apply("del8", "del8").unwrap();
    assert_eq!(settings.del, Some(4));

    settings.apply("deltasum5", "deltasum5").unwrap();
    assert_eq!(settings.deltasum, Some(4));

    settings.apply("exit6", "exit6").unwrap();
    assert_eq!(settings.exit, Some(4));

    settings.apply("filter5", "filter5").unwrap();
    assert_eq!(settings.filter, Some(4));

    settings.apply("flist9", "flist9").unwrap();
    assert_eq!(settings.flist, Some(4));

    settings.apply("fuzzy5", "fuzzy5").unwrap();
    assert_eq!(settings.fuzzy, Some(4));

    settings.apply("hlink5", "hlink5").unwrap();
    assert_eq!(settings.hlink, Some(4));

    settings.apply("iconv7", "iconv7").unwrap();
    assert_eq!(settings.iconv, Some(4));

    settings.apply("io5", "io5").unwrap();
    assert_eq!(settings.io, Some(4));

    settings.apply("own9", "own9").unwrap();
    assert_eq!(settings.own, Some(4));

    settings.apply("time6", "time6").unwrap();
    assert_eq!(settings.time, Some(4));
}

/// upstream: options.c:462-463 - "all" with a numeric suffix sets every flag to
/// min(suffix, MAX_OUT_LEVEL). e.g. `all4` sets all to 4, `all9` clamps to 4.
#[test]
fn debug_flag_all_with_level() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all4", "all4").unwrap();
    assert_eq!(settings.io, Some(4));
    assert_eq!(settings.flist, Some(4));
    assert_eq!(settings.hlink, Some(4));
    assert_eq!(settings.acl, Some(4));

    // Level beyond MAX_OUT_LEVEL is clamped.
    let mut settings = DebugFlagSettings::default();
    settings.apply("all9", "all9").unwrap();
    assert_eq!(settings.io, Some(4));
    assert_eq!(settings.hlink, Some(4));
}

#[test]
fn debug_flag_no_prefix_all_flags() {
    let keywords = [
        "acl", "backup", "bind", "chdir", "connect", "cmd", "del", "deltasum", "dup", "exit",
        "filter", "flist", "fuzzy", "genr", "hash", "hlink", "iconv", "io", "nstr", "own", "proto",
        "recv", "send", "time",
    ];
    for keyword in &keywords {
        let mut settings = DebugFlagSettings::default();
        let negated = format!("no{keyword}");
        let result = settings.apply(&negated, &negated);
        assert!(
            result.is_ok(),
            "negated debug keyword 'no{keyword}' should be accepted but got: {result:?}"
        );
    }
}

#[test]
fn debug_flag_numeric_1_enables_all() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("1", "1").unwrap();
    assert_eq!(settings.io, Some(1));
    assert_eq!(settings.proto, Some(1));
    assert_eq!(settings.flist, Some(1));
    assert_eq!(settings.acl, Some(1));
}

#[test]
fn debug_flag_numeric_0_disables_all() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("all", "all").unwrap();
    settings.apply("0", "0").unwrap();
    assert_eq!(settings.io, Some(0));
    assert_eq!(settings.proto, Some(0));
    assert_eq!(settings.flist, Some(0));
    assert_eq!(settings.acl, Some(0));
}

#[test]
fn debug_flag_iter_enabled_flags_returns_nonzero() {
    let mut settings = DebugFlagSettings::default();
    settings.apply("io2", "io2").unwrap();
    settings.apply("flist", "flist").unwrap();
    settings.apply("del0", "del0").unwrap();

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
fn info_flag_no_prefix_still_accepted_after_help_scrub() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("noprogress", "noprogress").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Disabled);
}

#[test]
fn info_flag_dash_prefix_still_accepted_after_help_scrub() {
    let mut settings = InfoFlagSettings::default();
    settings.apply("-progress", "-progress").unwrap();
    assert_eq!(settings.progress, ProgressSetting::Disabled);
}

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
// so the summary lists levels 2-5 only (options.c:238-245).
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
    settings.apply("2", "2").unwrap();
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
    settings.apply("3", "3").unwrap();
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
    settings.apply("2", "2").unwrap();
    settings.apply("name0", "name0").unwrap();
    assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
    // Other flags retained from the numeric pre-fill.
    assert_eq!(settings.stats, Some(2));
}

#[test]
fn parse_info_flags_numeric_then_named_in_one_arg() {
    let values = vec![OsString::from("2,name0")];
    let result = parse_info_flags(&values).unwrap();
    assert_eq!(result.name, Some(NameOutputLevel::Disabled));
    assert_eq!(result.stats, Some(2));
    assert_eq!(result.flist, Some(2));
}

#[test]
fn info_flag_numeric_high_value_saturates() {
    // Out-of-range integers saturate at per-flag caps rather than erroring.
    let mut settings = InfoFlagSettings::default();
    settings.apply("99", "99").unwrap();
    assert_eq!(settings.stats, Some(3));
    assert_eq!(settings.flist, Some(2));
    assert_eq!(settings.copy, Some(1));
}

#[test]
fn info_flag_numeric_overflow_does_not_panic() {
    // A value that overflows u8 falls back to u8::MAX inside the parser,
    // which still saturates to per-flag caps.
    let mut settings = InfoFlagSettings::default();
    settings.apply("999", "999").unwrap();
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
    settings.apply("2", "2").unwrap();
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
    settings.apply("copy", "copy").unwrap();
    settings.apply("del", "del").unwrap();
    settings.apply("flist2", "flist2").unwrap();
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
    settings.apply("all", "all").unwrap();
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
    settings.apply("none", "none").unwrap();
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
    settings.apply("2", "2").unwrap();
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
    settings.apply("progress2", "progress2").unwrap();
    settings.apply_to_thread_local();

    assert!(logging::info_gte(logging::InfoFlag::Progress, 2));

    // Reset and test progress disabled
    logging::init(logging::VerbosityConfig::from_verbose_level(1));
    let mut settings = InfoFlagSettings::default();
    settings.apply("progress0", "progress0").unwrap();
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
