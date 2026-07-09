
#[test]
fn parse_filter_directive_show_is_sender_only() {
    let rule = match parse_filter_directive_line("show images/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {other:?}"),
    };

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_hide_is_sender_only() {
    let rule = match parse_filter_directive_line("hide *.tmp").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {other:?}"),
    };

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_shorthand_show_is_sender_only() {
    let rule = match parse_filter_directive_line("S logs/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {other:?}"),
    };

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_shorthand_hide_is_sender_only() {
    let rule = match parse_filter_directive_line("H *.bak").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {other:?}"),
    };

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_shorthand_protect_requires_receiver() {
    let rule = match parse_filter_directive_line("P cache/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {other:?}"),
    };

    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_risk_requires_receiver() {
    let rule = match parse_filter_directive_line("risk cache/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {other:?}"),
    };

    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_keyword_with_xattr_modifier() {
    let rule = match parse_filter_directive_line("include,x user.keep").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {other:?}"),
    };

    assert!(rule.is_xattr_only());
    assert!(rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
    assert_eq!(rule.pattern(), "user.keep");
}

#[test]
fn parse_filter_directive_rejects_xattr_on_show_keyword() {
    let error = parse_filter_directive_line("show,x user.skip")
        .expect_err("show keyword should reject xattr modifier");
    assert!(error
        .to_string()
        .contains("uses unsupported modifier 'x'"));
}

#[test]
fn parse_filter_directive_shorthand_risk_requires_receiver() {
    let rule = match parse_filter_directive_line("R cache/**").expect("parse") {
        Some(ParsedFilterDirective::Rule(rule)) => rule,
        other => panic!("expected rule, got {other:?}"),
    };

    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn parse_filter_directive_clear_keyword() {
    let directive = parse_filter_directive_line("clear").expect("parse clear");
    assert!(matches!(directive, Some(ParsedFilterDirective::Clear)));

    let uppercase = parse_filter_directive_line("  CLEAR  ").expect("parse uppercase");
    assert!(matches!(uppercase, Some(ParsedFilterDirective::Clear)));

    let bang = parse_filter_directive_line("!").expect("parse bang");
    assert!(matches!(bang, Some(ParsedFilterDirective::Clear)));
}

#[test]
fn parse_filter_directive_exclude_if_present_support() {
    let directive = parse_filter_directive_line("exclude-if-present=.git")
        .expect("parse")
        .expect("directive");

    match directive {
        ParsedFilterDirective::ExcludeIfPresent(rule) => {
            assert_eq!(rule.marker_path(Path::new(".")), PathBuf::from("./.git"));
        }
        other => panic!("expected exclude-if-present directive, got {other:?}"),
    }
}

#[test]
fn parse_filter_directive_dir_merge_without_modifiers() {
    let directive = parse_filter_directive_line("dir-merge .rsync-filter")
        .expect("parse")
        .expect("directive");

    let (pattern, options) = match directive {
        ParsedFilterDirective::DirMerge { pattern, options } => (pattern, options),
        other => panic!("expected dir-merge directive, got {other:?}"),
    };

    assert_eq!(pattern, PathBuf::from(".rsync-filter"));
    assert!(options.inherit_rules());
    assert!(options.allows_comments());
    assert!(!options.uses_whitespace());
    assert_eq!(options.enforced_kind(), None);
}

#[test]
fn parse_filter_directive_per_dir_alias_without_modifiers() {
    let directive = parse_filter_directive_line("per-dir .rsync-filter")
        .expect("parse")
        .expect("directive");

    let (pattern, options) = match directive {
        ParsedFilterDirective::DirMerge { pattern, options } => (pattern, options),
        other => panic!("expected dir-merge directive, got {other:?}"),
    };

    assert_eq!(pattern, PathBuf::from(".rsync-filter"));
    assert!(options.inherit_rules());
    assert!(options.allows_comments());
    assert!(!options.uses_whitespace());
    assert_eq!(options.enforced_kind(), None);
}

#[test]
fn parse_filter_directive_dir_merge_with_modifiers() {
    let directive = parse_filter_directive_line("dir-merge,+ne rules/filter.txt")
        .expect("parse")
        .expect("directive");

    let (pattern, options) = match directive {
        ParsedFilterDirective::DirMerge { pattern, options } => (pattern, options),
        other => panic!("expected dir-merge directive, got {other:?}"),
    };

    assert_eq!(pattern, PathBuf::from("rules/filter.txt"));
    assert!(!options.inherit_rules());
    assert!(options.excludes_self());
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Include));
}

#[test]
fn parse_filter_directive_dir_merge_cvs_default_path() {
    let directive = parse_filter_directive_line("dir-merge,c")
        .expect("parse")
        .expect("directive");

    let (pattern, options) = match directive {
        ParsedFilterDirective::DirMerge { pattern, options } => (pattern, options),
        other => panic!("expected dir-merge directive, got {other:?}"),
    };

    assert_eq!(pattern, PathBuf::from(".cvsignore"));
    assert!(!options.inherit_rules());
    assert!(options.list_clear_allowed());
    assert!(options.uses_whitespace());
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
}

#[test]
fn parse_filter_directive_short_merge_inherits_context() {
    let directive = parse_filter_directive_line(". per-dir")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected merge directive, got {other:?}"),
    };

    assert_eq!(path, PathBuf::from("per-dir"));
    assert!(options.is_none());
}

#[test]
fn parse_filter_directive_short_merge_cvs_defaults() {
    let directive = parse_filter_directive_line(".C")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected merge directive, got {other:?}"),
    };

    assert_eq!(path, PathBuf::from(".cvsignore"));
    let opts = options.expect("options");
    assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    assert!(opts.uses_whitespace());
    assert!(!opts.inherit_rules());
}

#[test]
fn parse_filter_directive_short_dir_merge_with_modifiers() {
    let directive = parse_filter_directive_line(":- per-dir")
        .expect("parse")
        .expect("directive");

    let (pattern, options) = match directive {
        ParsedFilterDirective::DirMerge { pattern, options } => (pattern, options),
        other => panic!("expected dir-merge directive, got {other:?}"),
    };

    assert_eq!(pattern, PathBuf::from("per-dir"));
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
}

#[test]
fn parse_filter_directive_merge_with_modifiers() {
    let directive = parse_filter_directive_line("merge,+ rules")
        .expect("parse")
        .expect("directive");

    let (path, options) = match directive {
        ParsedFilterDirective::Merge { path, options } => (path, options),
        other => panic!("expected merge directive, got {other:?}"),
    };

    assert_eq!(path, PathBuf::from("rules"));
    let opts = options.expect("options");
    assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Include));
}

#[test]
fn parse_filter_directive_dir_merge_conflicting_modifiers_error() {
    let error = parse_filter_directive_line("dir-merge,+- rules").expect_err("conflict");
    assert!(
        error
            .to_string()
            .contains("cannot combine '+' and '-' modifiers")
    );
}

#[test]
fn deferred_updates_flush_commits_pending_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"payload").expect("write source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&destination_root).expect("create dest root");
    let destination = destination_root.join("file.txt");

    let options = LocalCopyOptions::default()
        .partial(true)
        .delay_updates(true);
    let mut context = CopyContext::new(
        LocalCopyExecution::Apply,
        options,
        None,
        destination_root,
    );

    let (guard, mut file) =
        DestinationWriteGuard::new(destination.as_path(), true, None, None).expect("guard");
    file.write_all(b"payload").expect("write temp");
    drop(file);

    let metadata = fs::metadata(&source).expect("metadata");
    let metadata_options = context.metadata_options();
    // upstream: the staging temp is `.name.XXXXXX` beside the destination and is
    // renamed onto the destination when the deferred update flushes.
    let staging_path = guard.staging_path().to_path_buf();
    let final_path = guard.final_path().to_path_buf();
    let update = DeferredUpdate::new(
        guard,
        metadata.clone(),
        metadata_options,
        LocalCopyExecution::Apply,
        OwnedPathContext {
            source,
            relative: Some(std::path::PathBuf::from("file.txt")),
            file_type: metadata.file_type(),
            destination_previously_existed: false,
        },
        final_path,
        #[cfg(all(unix, feature = "xattr"))]
        context.xattrs_enabled(),
        #[cfg(all(any(unix, windows), feature = "acl"))]
        context.acls_enabled(),
    );

    context.register_deferred_update(update);

    assert!(!destination.exists());
    assert!(staging_path.exists());

    context
        .flush_deferred_updates()
        .expect("deferred updates committed");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"payload");
    assert!(!staging_path.exists());
}

#[test]
fn dir_merge_defaults_preserve_rule_side_overrides() {
    let options = DirMergeOptions::default();
    let rule = FilterRule::exclude("*.tmp").with_receiver(false);
    let adjusted = apply_dir_merge_rule_defaults(rule, &options, false);

    assert!(adjusted.applies_to_sender());
    assert!(!adjusted.applies_to_receiver());
}

#[test]
fn dir_merge_modifiers_override_rule_side_overrides() {
    let sender_only_options = DirMergeOptions::default().sender_modifier();
    let receiver_only_options = DirMergeOptions::default().receiver_modifier();

    let rule = FilterRule::include("logs/**").with_receiver(false);
    let sender_adjusted =
        apply_dir_merge_rule_defaults(rule.clone(), &sender_only_options, false);
    assert!(sender_adjusted.applies_to_sender());
    assert!(!sender_adjusted.applies_to_receiver());

    let receiver_adjusted = apply_dir_merge_rule_defaults(rule, &receiver_only_options, false);
    assert!(!receiver_adjusted.applies_to_sender());
    assert!(receiver_adjusted.applies_to_receiver());
}

/// upstream: exclude.c:1324-1332 parse_rule_tok - rules expanded from a
/// merge file under --delete-excluded carry FILTRULE_SENDER_SIDE when
/// neither side modifier is set on the wrapper. Per-token rules from
/// `:C .cvsignore` or built-in CVS patterns must follow the same rule so
/// the receiver's delete-pass does not skip them.
#[test]
fn dir_merge_defaults_apply_implicit_sender_side_under_delete_excluded() {
    let options = DirMergeOptions::default();
    let rule = FilterRule::exclude("*.bak");
    let adjusted = apply_dir_merge_rule_defaults(rule, &options, true);

    assert!(adjusted.applies_to_sender());
    assert!(!adjusted.applies_to_receiver());
}

/// Explicit side modifiers on the dir-merge wrapper take precedence over
/// the implicit `--delete-excluded` flip, mirroring upstream's check on
/// FILTRULES_SIDES in `exclude.c:1330`.
#[test]
fn dir_merge_defaults_respect_explicit_side_modifier_under_delete_excluded() {
    let receiver_only = DirMergeOptions::default().receiver_modifier();
    let rule = FilterRule::exclude("*.bak");
    let adjusted = apply_dir_merge_rule_defaults(rule, &receiver_only, true);

    assert!(!adjusted.applies_to_sender());
    assert!(adjusted.applies_to_receiver());
}

/// Protect and risk rules are receiver-only by construction; the implicit
/// flip must not strip the receiver flag from them.
#[test]
fn dir_merge_defaults_skip_protect_risk_under_delete_excluded() {
    let options = DirMergeOptions::default();
    let protect = FilterRule::protect("keep");
    let adjusted = apply_dir_merge_rule_defaults(protect, &options, true);
    assert!(!adjusted.applies_to_sender());
    assert!(adjusted.applies_to_receiver());

    let risk = FilterRule::risk("scratch");
    let adjusted = apply_dir_merge_rule_defaults(risk, &options, true);
    assert!(!adjusted.applies_to_sender());
    assert!(adjusted.applies_to_receiver());
}
