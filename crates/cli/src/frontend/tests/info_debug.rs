use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn info_progress2_enables_progress_output() {
    use std::os::unix::fs::FileTypeExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-fifo.in");
    mkfifo_for_tests(&source, 0o600).expect("mkfifo");

    let destination = tmp.path().join("info-fifo.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        OsString::from("--specials"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(!rendered.contains("info-fifo.in"));
    assert!(rendered.contains("to-chk=0/1"));
    assert!(rendered.contains("0.00kB/s"));

    let metadata = std::fs::symlink_metadata(&destination).expect("stat destination");
    assert!(metadata.file_type().is_fifo());
}

#[test]
fn info_stats_enables_summary_block() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-stats.txt");
    let destination = tmp.path().join("info-stats.out");
    let payload = b"statistics";
    std::fs::write(&source, payload).expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=stats"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("stats output is UTF-8");
    let expected_size = payload.len();
    assert!(rendered.contains("Number of files: 1 (reg: 1)"));
    assert!(rendered.contains(&format!("Total file size: {expected_size} bytes")));
    assert!(rendered.contains("Literal data:"));
    assert!(rendered.contains("\n\nsent"));
    assert!(rendered.contains("total size is"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        payload
    );
}

#[test]
fn info_none_disables_progress_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-none.txt");
    let destination = tmp.path().join("info-none.out");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--info=none"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(!rendered.contains("to-chk"));
    assert!(rendered.trim().is_empty());
}

#[test]
fn info_help_lists_supported_flags() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--info=help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout, INFO_HELP_TEXT.as_bytes());
}

#[test]
fn debug_help_lists_supported_flags() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--debug=help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout, DEBUG_HELP_TEXT.as_bytes());
}

#[test]
fn info_rejects_unknown_flag() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--info=unknown")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("stderr utf8");
    assert!(rendered.contains("invalid --info flag"));
}

#[test]
fn info_accepts_comma_separated_tokens() {
    let flags = vec![OsString::from("progress,name2,stats")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert!(matches!(settings.progress, ProgressSetting::PerFile));
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
    assert_eq!(settings.stats, Some(1));
}

#[test]
fn info_backup_flag_parsing() {
    let flags = vec![OsString::from("backup")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.backup, Some(1));

    let flags = vec![OsString::from("backup0")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.backup, Some(0));

    let flags = vec![OsString::from("nobackup")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.backup, Some(0));
}

#[test]
fn info_flist_levels() {
    let flags = vec![OsString::from("flist")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.flist, Some(1));

    let flags = vec![OsString::from("flist2")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.flist, Some(2));

    let flags = vec![OsString::from("flist3")];
    let error = parse_info_flags(&flags)
        .err()
        .expect("should reject level 3");
    assert!(error.to_string().contains("invalid --info flag"));
}

#[test]
fn info_stats_levels() {
    let flags = vec![OsString::from("stats")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.stats, Some(1));

    let flags = vec![OsString::from("stats2")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.stats, Some(2));

    let flags = vec![OsString::from("stats3")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.stats, Some(3));

    let flags = vec![OsString::from("stats4")];
    let error = parse_info_flags(&flags)
        .err()
        .expect("should reject level 4");
    assert!(error.to_string().contains("invalid --info flag"));
}

#[test]
fn info_negation_forms() {
    // Test 'no' prefix
    let flags = vec![OsString::from("nodel")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.del, Some(0));

    // Test '-' prefix
    let flags = vec![OsString::from("-skip")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.skip, Some(0));

    // Test '0' suffix
    let flags = vec![OsString::from("copy0")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.copy, Some(0));
}

#[test]
fn info_rejects_empty_segments() {
    let flags = vec![OsString::from("progress,,stats")];
    let error = parse_info_flags(&flags).err().expect("parse should fail");
    assert!(error.to_string().contains("--info flag must not be empty"));
}

#[test]
fn debug_accepts_comma_separated_tokens() {
    let flags = vec![OsString::from("io,proto")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert!(!settings.help_requested);
    assert_eq!(settings.io, Some(1));
    assert_eq!(settings.proto, Some(1));
}

#[test]
fn debug_flist_levels() {
    let flags = vec![OsString::from("flist")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.flist, Some(1));

    let flags = vec![OsString::from("flist2")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.flist, Some(2));

    let flags = vec![OsString::from("flist4")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.flist, Some(4));

    let flags = vec![OsString::from("flist5")];
    let error = parse_debug_flags(&flags)
        .err()
        .expect("should reject level 5");
    assert!(error.to_string().contains("invalid --debug flag"));
}

#[test]
fn debug_io_levels() {
    let flags = vec![OsString::from("io")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.io, Some(1));

    let flags = vec![OsString::from("io3")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.io, Some(3));

    let flags = vec![OsString::from("io4")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.io, Some(4));

    let flags = vec![OsString::from("io5")];
    let error = parse_debug_flags(&flags)
        .err()
        .expect("should reject level 5");
    assert!(error.to_string().contains("invalid --debug flag"));
}

#[test]
fn debug_negation_forms() {
    // Test 'no' prefix
    let flags = vec![OsString::from("nodel")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.del, Some(0));

    // Test '-' prefix
    let flags = vec![OsString::from("-filter")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.filter, Some(0));

    // Test '0' suffix
    let flags = vec![OsString::from("recv0")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.recv, Some(0));
}

#[test]
fn debug_all_and_none() {
    let flags = vec![OsString::from("all")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.io, Some(1));
    assert_eq!(settings.proto, Some(1));
    assert_eq!(settings.flist, Some(1));

    let flags = vec![OsString::from("none")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert_eq!(settings.io, Some(0));
    assert_eq!(settings.proto, Some(0));
    assert_eq!(settings.flist, Some(0));
}

#[test]
fn debug_rejects_empty_segments() {
    let flags = vec![OsString::from("deltasum,,io")];
    let error = parse_debug_flags(&flags).err().expect("parse should fail");
    assert!(error.to_string().contains("--debug flag must not be empty"));
}

#[test]
fn info_name_emits_filenames_without_verbose() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("name.txt");
    let destination = tmp.path().join("name.out");
    std::fs::write(&source, b"name-info").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=name"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(rendered.contains("name.txt"));
    assert!(rendered.contains("sent"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"name-info"
    );
}

#[test]
fn info_name0_suppresses_verbose_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("quiet.txt");
    let destination = tmp.path().join("quiet.out");
    std::fs::write(&source, b"quiet").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--info=name0"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(!rendered.contains("quiet.txt"));
    assert!(rendered.contains("sent"));
}

#[test]
fn info_name2_reports_unchanged_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("unchanged.txt");
    let destination = tmp.path().join("unchanged.out");
    std::fs::write(&source, b"unchanged").expect("write source");

    let initial = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);
    assert_eq!(initial.0, 0);
    assert!(initial.1.is_empty());
    assert!(initial.2.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=name2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(rendered.contains("unchanged.txt"));
}

// ============================================================================
// End-to-end tests: --info flag via CLI argument parsing
// ============================================================================

#[test]
fn info_flag_captured_in_parsed_args() {
    let parsed = crate::frontend::arguments::parse_args(
        ["rsync", "--info=name", "src/", "dst/"]
            .iter()
            .map(|s| s.to_string()),
    )
    .expect("parse");
    assert!(!parsed.info.is_empty());
}

#[test]
fn info_flag_multiple_values_captured() {
    let parsed = crate::frontend::arguments::parse_args(
        ["rsync", "--info=name", "--info=stats2", "src/", "dst/"]
            .iter()
            .map(|s| s.to_string()),
    )
    .expect("parse");
    // With value_delimiter(','), clap splits tokens individually
    assert!(parsed.info.len() >= 2);
}

#[test]
fn info_flag_comma_separated_captured() {
    let parsed = crate::frontend::arguments::parse_args(
        ["rsync", "--info=name,stats2,copy", "src/", "dst/"]
            .iter()
            .map(|s| s.to_string()),
    )
    .expect("parse");
    // clap with value_delimiter splits "name,stats2,copy" into 3 values
    assert!(parsed.info.len() >= 3);
}

#[test]
fn debug_flag_captured_in_parsed_args() {
    let parsed = crate::frontend::arguments::parse_args(
        ["rsync", "--debug=io", "src/", "dst/"]
            .iter()
            .map(|s| s.to_string()),
    )
    .expect("parse");
    assert!(!parsed.debug.is_empty());
}

// ============================================================================
// End-to-end tests: error handling via full CLI pipeline
// ============================================================================

#[test]
fn info_unknown_flag_exit_code_1() {
    let (code, _stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--info=notaflag")]);
    assert_eq!(code, 1);
    let rendered = String::from_utf8(stderr).expect("stderr utf8");
    assert!(rendered.contains("invalid --info flag"));
    assert!(rendered.contains("notaflag"));
}

#[test]
fn debug_rejects_unknown_flag() {
    let (code, _stdout, stderr) =
        run_with_args([OsStr::new(RSYNC), OsStr::new("--debug=notaflag")]);
    assert_eq!(code, 1);
    let rendered = String::from_utf8(stderr).expect("stderr utf8");
    assert!(rendered.contains("invalid --debug flag"));
}

// ============================================================================
// End-to-end tests: info flag interaction with verbose levels
// ============================================================================

#[test]
fn info_stats0_suppresses_verbose_stats() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("nostats.txt");
    let destination = tmp.path().join("nostats.out");
    std::fs::write(&source, b"nostats").expect("write source");

    // -v normally enables stats, but --info=stats0 should suppress it
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--info=stats0"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    // Stats block should not appear
    assert!(!rendered.contains("Number of files:"));
}

#[test]
fn info_all_enables_comprehensive_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("all.txt");
    let destination = tmp.path().join("all.out");
    std::fs::write(&source, b"all-info").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=all"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    // all should enable name output and stats
    assert!(rendered.contains("all.txt"));
    assert!(rendered.contains("sent"));
}

#[test]
fn info_none_suppresses_verbose_name_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("none.txt");
    let destination = tmp.path().join("none.out");
    std::fs::write(&source, b"none-test").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--info=none"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    // --info=none should suppress filename output that -v normally enables
    assert!(!rendered.contains("none.txt"));
}

// ============================================================================
// Comprehensive unit tests for all --info flag keywords
// ============================================================================

#[test]
fn info_all_upstream_keywords_are_accepted() {
    let keywords = [
        "backup", "copy", "del", "flist", "misc", "mount", "name", "nonreg", "progress", "remove",
        "skip", "stats", "symsafe",
    ];
    for keyword in &keywords {
        let flags = vec![OsString::from(*keyword)];
        let result = parse_info_flags(&flags);
        assert!(
            result.is_ok(),
            "info keyword '{keyword}' should be accepted"
        );
    }
}

#[test]
fn info_all_upstream_keywords_with_level_0() {
    let keywords = [
        "backup0",
        "copy0",
        "del0",
        "flist0",
        "misc0",
        "mount0",
        "name0",
        "nonreg0",
        "progress0",
        "remove0",
        "skip0",
        "stats0",
        "symsafe0",
    ];
    for keyword in &keywords {
        let flags = vec![OsString::from(*keyword)];
        let result = parse_info_flags(&flags);
        assert!(
            result.is_ok(),
            "info keyword '{keyword}' with level 0 should be accepted"
        );
    }
}

#[test]
fn info_all_upstream_keywords_with_negation() {
    let keywords = [
        "nobackup",
        "nocopy",
        "nodel",
        "noflist",
        "nomisc",
        "nomount",
        "noname",
        "nononreg",
        "noprogress",
        "noremove",
        "noskip",
        "nostats",
        "nosymsafe",
    ];
    for keyword in &keywords {
        let flags = vec![OsString::from(*keyword)];
        let result = parse_info_flags(&flags);
        assert!(
            result.is_ok(),
            "info keyword '{keyword}' with no-prefix should be accepted"
        );
    }
}

#[test]
fn info_all_upstream_keywords_with_dash_negation() {
    let keywords = [
        "-backup",
        "-copy",
        "-del",
        "-flist",
        "-misc",
        "-mount",
        "-name",
        "-nonreg",
        "-progress",
        "-remove",
        "-skip",
        "-stats",
        "-symsafe",
    ];
    for keyword in &keywords {
        let flags = vec![OsString::from(*keyword)];
        let result = parse_info_flags(&flags);
        assert!(
            result.is_ok(),
            "info keyword '{keyword}' with dash-prefix should be accepted"
        );
    }
}

// ============================================================================
// Upstream rsync specific patterns
// ============================================================================

#[test]
fn info_typical_rsync_quiet_pattern() {
    // rsync --info=none -- suppress everything
    let flags = vec![OsString::from("none")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.progress, ProgressSetting::Disabled);
    assert_eq!(settings.stats, Some(0));
    assert_eq!(settings.name, Some(NameOutputLevel::Disabled));
}

#[test]
fn info_typical_rsync_verbose_pattern() {
    // rsync --info=flist2,name2,del,copy,misc2 -- like -vv
    let flags = vec![OsString::from("flist2,name2,del,copy,misc2")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.flist, Some(2));
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
    assert_eq!(settings.del, Some(1));
    assert_eq!(settings.copy, Some(1));
    assert_eq!(settings.misc, Some(2));
}

#[test]
fn info_typical_rsync_progress_only() {
    // rsync --info=progress2 -- overall progress only
    let flags = vec![OsString::from("progress2")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.progress, ProgressSetting::Overall);
    // Other flags not set
    assert_eq!(settings.name, None);
    assert_eq!(settings.stats, None);
}

#[test]
fn info_typical_rsync_stats_only() {
    // rsync --info=stats2 -- detailed stats only
    let flags = vec![OsString::from("stats2")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.stats, Some(2));
    assert_eq!(settings.name, None);
    assert_eq!(settings.progress, ProgressSetting::default());
}

#[test]
fn info_case_sensitive_flag_names() {
    // Flag names should be case-insensitive in the CLI flags parser
    // (the apply function lowercases before matching)
    let flags = vec![OsString::from("STATS")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.stats, Some(1));
}

#[test]
fn info_mixed_case_keyword_accepted() {
    let flags = vec![OsString::from("Stats")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert_eq!(settings.stats, Some(1));
}
