use crate::message::strings;

#[test]
fn message_scratch_with_thread_local_borrows_shared_buffer() {
    let len = MessageScratch::with_thread_local(|scratch| {
        let message = Message::error(23, "delta-transfer failure")
            .with_role(Role::Sender)
            .with_source(message_source!());
        message.as_segments(scratch, false).len()
    });

    assert!(len > 0);
}

#[test]
fn message_scratch_with_thread_local_reenters_with_fresh_buffer() {
    MessageScratch::with_thread_local(|outer| {
        let outer_addr = outer as *mut MessageScratch as usize;

        let nested_len = MessageScratch::with_thread_local(|inner| {
            let inner_addr = inner as *mut MessageScratch as usize;
            assert_ne!(
                inner_addr, outer_addr,
                "reentrant borrow should receive a distinct scratch buffer"
            );

            let message = Message::info("nested reentry");
            message.as_segments(inner, false).len()
        });

        assert!(nested_len > 0);

        let outer_len = Message::warning("outer after reentry")
            .as_segments(outer, false)
            .len();
        assert!(outer_len > 0);
    });
}

#[test]
fn message_new_allows_dynamic_severity() {
    let warning = Message::new(Severity::Warning, "dynamic warning");
    assert_eq!(warning.severity(), Severity::Warning);
    assert_eq!(warning.code(), None);
    assert_eq!(warning.text(), "dynamic warning");

    let error = Message::new(Severity::Error, "dynamic error").with_code(23);
    assert_eq!(error.severity(), Severity::Error);
    assert_eq!(error.code(), Some(23));
    assert_eq!(error.text(), "dynamic error");
}

#[test]
fn message_from_exit_code_rehydrates_known_entries() {
    let error = Message::from_exit_code(23).expect("exit code 23 is defined");
    assert_eq!(error.severity(), Severity::Error);
    assert_eq!(error.code(), Some(23));
    assert_eq!(
        error.text(),
        "some files/attrs were not transferred (see previous errors)"
    );

    let warning = Message::from_exit_code(24).expect("exit code 24 is defined");
    assert!(warning.is_warning());
    assert_eq!(warning.code(), Some(24));
    assert_eq!(
        warning.text(),
        "some files vanished before they could be transferred"
    );

    let rendered = warning.to_string();
    assert!(rendered.starts_with("rsync warning: some files vanished"));
    assert!(rendered.contains("(code 24)"));
}

#[test]
fn message_from_exit_code_returns_none_for_unknown_values() {
    for code in [-1, 0, 7, 200] {
        assert!(
            Message::from_exit_code(code).is_none(),
            "unexpected mapping for {code}"
        );
    }
}

#[test]
fn exit_code_message_with_detail_includes_canonical_text() {
    let message = strings::exit_code_message_with_detail(1, "unrecognised flag --bad")
        .expect("exit code 1 should be defined");

    assert_eq!(message.code(), Some(1));
    assert!(
        message.text().starts_with("syntax or usage error: "),
        "message should reuse the canonical exit-code wording"
    );
    assert!(
        message.text().contains("unrecognised flag --bad"),
        "detail text should be appended"
    );
}

#[test]
fn message_with_text_replaces_payload_without_touching_metadata() {
    let original = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let updated = original
        .clone()
        .with_text("retry scheduled for delta-transfer");

    assert_eq!(updated.text(), "retry scheduled for delta-transfer");
    assert_eq!(updated.code(), Some(23));
    assert_eq!(updated.role(), Some(Role::Sender));
    assert_eq!(updated.source(), original.source());
    assert_eq!(original.text(), "delta-transfer failure");
}

#[test]
fn message_parts_provides_borrowed_views() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let (severity, code, text, role, source) = message.parts();

    assert_eq!(severity, Severity::Error);
    assert_eq!(code, Some(11));
    assert_eq!(text, "error in file IO");
    assert_eq!(role, Some(Role::Receiver));
    assert_eq!(source, message.source());

    // Ensure the original message is still usable after inspecting it.
    assert!(
        message
            .to_string()
            .contains("rsync error: error in file IO")
    );
}

#[test]
fn message_into_parts_transfers_owned_components() {
    let message = Message::warning("vanished files detected")
        .with_code(24)
        .with_role(Role::Generator)
        .with_source(message_source!());

    let (severity, code, text, role, source) = message.into_parts();

    assert_eq!(severity, Severity::Warning);
    assert_eq!(code, Some(24));
    assert_eq!(text, Cow::Borrowed("vanished files detected"));
    assert_eq!(role, Some(Role::Generator));
    assert!(source.is_some());
}

#[test]
fn message_with_severity_reclassifies_without_touching_metadata() {
    let original = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let downgraded = original.clone().with_severity(Severity::Warning);

    assert_eq!(downgraded.severity(), Severity::Warning);
    assert_eq!(downgraded.code(), original.code());
    assert_eq!(downgraded.role(), original.role());
    assert_eq!(downgraded.source(), original.source());
    assert_eq!(downgraded.text(), original.text());

    let rendered = downgraded.to_string();
    assert!(rendered.starts_with("rsync warning:"));
    assert!(rendered.contains("(code 23)"));
}

#[test]
fn message_predicates_forward_to_severity() {
    let info = Message::info("probe");
    assert!(info.is_info());
    assert!(!info.is_warning());
    assert!(!info.is_error());

    let warning = Message::warning("vanished");
    assert!(warning.is_warning());
    assert!(!warning.is_info());
    assert!(!warning.is_error());

    let error = Message::error(11, "io failure");
    assert!(error.is_error());
    assert!(!error.is_info());
    assert!(!error.is_warning());
}

#[test]
fn formats_error_with_code_role_and_source() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let formatted = message.to_string();

    assert!(formatted.starts_with("rsync error: delta-transfer failure (code 23) at "));
    assert!(formatted.contains(&format!("[sender={}]", crate::version::RUST_VERSION)));
    assert!(formatted.contains("src/message/tests.rs"));
}

#[test]
fn message_without_role_clears_trailer() {
    let formatted = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .without_role()
        .to_string();

    assert!(!formatted.contains("[sender="));
}

#[test]
fn message_without_source_clears_location() {
    let formatted = Message::error(23, "delta-transfer failure")
        .with_source(message_source!())
        .without_source()
        .to_string();

    assert!(!formatted.contains(" at "));
}

#[test]
fn message_without_code_clears_suffix() {
    let formatted = Message::error(23, "delta-transfer failure")
        .without_code()
        .to_string();

    assert!(!formatted.contains("(code"));
}

#[test]
fn formats_warning_without_role_or_source() {
    let message = Message::warning("soft limit reached");
    let formatted = message.to_string();

    assert_eq!(formatted, "rsync warning: soft limit reached");
}

#[test]
fn warnings_with_code_render_code_suffix() {
    let formatted = Message::warning("some files vanished before they could be transferred")
        .with_code(24)
        .to_string();

    assert!(formatted.starts_with("rsync warning: some files vanished"));
    assert!(formatted.contains("(code 24)"));
}

#[test]
fn info_messages_omit_code_suffix() {
    let message = Message::info("protocol handshake complete").with_source(message_source!());
    let formatted = message.to_string();

    assert!(formatted.starts_with("rsync info: protocol handshake complete at "));
    assert!(!formatted.contains("(code"));
}

#[test]
fn source_location_is_repo_relative() {
    let source = message_source!();
    let path = source.path();
    assert!(
        path.starts_with(TESTS_DIR),
        "expected {path:?} to start with {TESTS_DIR:?}"
    );
    assert!(
        path.ends_with(".rs"),
        "expected {path:?} to reference a Rust source file"
    );
    assert!(!path.contains('\\'));
    assert!(source.line() > 0);
    assert!(source.is_workspace_relative());
}

#[test]
fn normalizes_redundant_segments() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let source = SourceLocation::from_parts(manifest_dir, "src/../src/message/message_impl.rs", 7);
    assert_eq!(source.path(), "crates/core/src/message/message_impl.rs");
}

#[test]
fn include_shards_map_to_virtual_tests_entry_point() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let source = SourceLocation::from_parts(manifest_dir, "src/message/tests/part3.rs", 11);

    assert_eq!(source.path(), "crates/core/src/message/tests.rs");
    assert_eq!(source.line(), 11);
}

#[test]
fn retains_absolute_paths_outside_workspace() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let source = SourceLocation::from_parts(manifest_dir, "/tmp/outside.rs", 42);

    assert!(std::path::Path::new(source.path()).is_absolute());
    assert!(!source.is_workspace_relative());
}

#[test]
fn strips_workspace_prefix_after_normalization() {
    let workspace_root = std::path::Path::new(env!("RSYNC_WORKSPACE_ROOT"));
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    let crate_relative = manifest_dir
        .strip_prefix(workspace_root)
        .expect("manifest directory must live within the workspace root");

    let redundant_root = workspace_root.join("..").join(
        workspace_root
            .file_name()
            .expect("workspace root should have a terminal component"),
    );

    let redundant_path = redundant_root
        .join(crate_relative)
        .join("src/message/message_impl.rs");

    let leaked: &'static str = Box::leak(
        redundant_path
            .to_string_lossy()
            .into_owned()
            .into_boxed_str(),
    );

    let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), leaked, 7);
    assert_eq!(source.path(), "crates/core/src/message/message_impl.rs");
}

#[test]
fn workspace_prefix_match_requires_separator_boundary() {
    let workspace_root = Path::new(env!("RSYNC_WORKSPACE_ROOT"));

    let Some(root_name) = workspace_root.file_name() else {
        // When the workspace lives at the filesystem root (e.g. `/`), every absolute path
        // is a descendant. The existing behaviour already strips the prefix, so there is no
        // partial-prefix scenario to validate.
        return;
    };

    let sibling_name = format!("{}-fork", root_name.to_string_lossy());
    let sibling = workspace_root
        .parent()
        .unwrap_or(workspace_root)
        .join(&sibling_name)
        .join("src/lib.rs");

    let leaked: &'static str = Box::leak(sibling.to_string_lossy().into_owned().into_boxed_str());

    let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), leaked, 11);
    let expected = normalize_path(Path::new(leaked));

    assert_eq!(source.path(), expected);
    assert!(Path::new(source.path()).is_absolute());
}

