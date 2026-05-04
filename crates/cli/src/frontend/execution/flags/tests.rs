use super::debug::DebugFlagSettings;
use super::info::InfoFlagSettings;
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

#[test]
fn debug_flag_apply_io_level_too_high() {
    let mut settings = DebugFlagSettings::default();
    let result = settings.apply("io5", "io5");
    assert!(result.is_err());
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

#[test]
fn debug_flag_level_limits() {
    let mut settings = DebugFlagSettings::default();

    // backup: max 2
    assert!(settings.apply("backup2", "backup2").is_ok());
    assert!(settings.apply("backup3", "backup3").is_err());

    // connect: max 2
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("connect2", "connect2").is_ok());
    assert!(settings.apply("connect3", "connect3").is_err());

    // cmd: max 2
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("cmd2", "cmd2").is_ok());
    assert!(settings.apply("cmd3", "cmd3").is_err());

    // del: max 3
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("del3", "del3").is_ok());
    assert!(settings.apply("del4", "del4").is_err());

    // deltasum: max 4
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("deltasum4", "deltasum4").is_ok());
    assert!(settings.apply("deltasum5", "deltasum5").is_err());

    // exit: max 3
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("exit3", "exit3").is_ok());
    assert!(settings.apply("exit4", "exit4").is_err());

    // filter: max 3
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("filter3", "filter3").is_ok());
    assert!(settings.apply("filter4", "filter4").is_err());

    // flist: max 4
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("flist4", "flist4").is_ok());
    assert!(settings.apply("flist5", "flist5").is_err());

    // fuzzy: max 2
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("fuzzy2", "fuzzy2").is_ok());
    assert!(settings.apply("fuzzy3", "fuzzy3").is_err());

    // hlink: max 3
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("hlink3", "hlink3").is_ok());
    assert!(settings.apply("hlink4", "hlink4").is_err());

    // iconv: max 2
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("iconv2", "iconv2").is_ok());
    assert!(settings.apply("iconv3", "iconv3").is_err());

    // io: max 4
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("io4", "io4").is_ok());
    assert!(settings.apply("io5", "io5").is_err());

    // own: max 2
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("own2", "own2").is_ok());
    assert!(settings.apply("own3", "own3").is_err());

    // time: max 2
    let mut settings = DebugFlagSettings::default();
    assert!(settings.apply("time2", "time2").is_ok());
    assert!(settings.apply("time3", "time3").is_err());
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

#[test]
fn info_help_text_mentions_all_and_none() {
    assert!(INFO_HELP_TEXT.contains("all"));
    assert!(INFO_HELP_TEXT.contains("none"));
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

#[test]
fn debug_help_text_mentions_all_and_none() {
    assert!(DEBUG_HELP_TEXT.contains("all"));
    assert!(DEBUG_HELP_TEXT.contains("none"));
}
