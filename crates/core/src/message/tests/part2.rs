#[test]
fn strip_normalized_workspace_prefix_returns_current_dir_for_exact_match() {
    let root = "/workspace/project";
    let stripped = strip_normalized_workspace_prefix(root, root)
        .expect("identical paths should collapse to the current directory");

    assert_eq!(stripped, ".");
}

#[test]
fn strip_normalized_workspace_prefix_accepts_trailing_separator_on_root() {
    let root = "/workspace/project/";
    let path = "/workspace/project/crates/core/src/lib.rs";
    let stripped = strip_normalized_workspace_prefix(path, root)
        .expect("child paths should remain accessible when the root ends with a separator");

    assert_eq!(stripped, "crates/core/src/lib.rs");
}

#[test]
fn strip_normalized_workspace_prefix_rejects_partial_component_matches() {
    let root = "/workspace/project";
    let path = "/workspace/project-old/src/lib.rs";

    assert!(
        strip_normalized_workspace_prefix(path, root).is_none(),
        "differing path segments must not be treated as the same workspace",
    );
}

#[test]
fn escaping_workspace_root_renders_absolute_path() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let escape = Path::new("../../../../outside.rs");
    let absolute = manifest_dir.join(escape);

    let leaked: &'static str = Box::leak(escape.to_string_lossy().into_owned().into_boxed_str());

    let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), leaked, 13);

    assert!(Path::new(source.path()).is_absolute());
    assert_eq!(source.path(), normalize_path(&absolute));
}

#[test]
fn workspace_root_path_is_marked_relative() {
    let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), ".", 5);

    assert_eq!(source.path(), "crates/core");
    assert!(source.is_workspace_relative());
}

#[test]
fn compute_workspace_root_prefers_explicit_env() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let computed = compute_workspace_root(Some(manifest), Some("ignored"))
        .expect("explicit manifest directory should be accepted");

    assert_eq!(computed, PathBuf::from(manifest));
}

#[test]
fn compute_workspace_root_falls_back_to_manifest_ancestors() {
    let expected = canonicalize_or_fallback(Path::new(env!("RSYNC_WORKSPACE_ROOT")));
    let computed =
        compute_workspace_root(None, None).expect("ancestor scan should locate the workspace root");

    assert_eq!(computed, expected);
}

#[test]
fn compute_workspace_root_handles_relative_workspace_dir() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = Path::new(env!("RSYNC_WORKSPACE_ROOT"));

    let relative_from_root = match manifest_dir.strip_prefix(workspace_root) {
        Ok(relative) => relative,
        Err(_) => Path::new("."),
    };

    let mut relative_to_root = PathBuf::new();
    for component in relative_from_root.components() {
        if matches!(component, std::path::Component::Normal(_)) {
            relative_to_root.push("..");
        }
    }

    if relative_to_root.as_os_str().is_empty() {
        relative_to_root.push(".");
    }

    let relative_owned = relative_to_root.to_string_lossy().into_owned();
    let computed = compute_workspace_root(None, Some(relative_owned.as_str()))
        .expect("relative workspace dir should resolve");
    let expected = canonicalize_or_fallback(workspace_root);

    assert_eq!(computed, expected);
}

#[test]
fn source_location_clone_preserves_path_and_line() {
    let original = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), "src/lib.rs", 42);
    let cloned = original.clone();

    assert_eq!(original, cloned);
    assert_eq!(cloned.path(), "crates/core/src/lib.rs");
    assert_eq!(cloned.line(), 42);
}

#[test]
fn normalize_preserves_relative_parent_segments() {
    let normalized = normalize_path(Path::new("../shared/src/lib.rs"));
    assert_eq!(normalized, "../shared/src/lib.rs");
}

#[test]
fn normalize_empty_path_defaults_to_current_dir() {
    let normalized = normalize_path(Path::new(""));
    assert_eq!(normalized, ".");
}

#[test]
fn normalize_windows_drive_paths_standardizes_separators() {
    let normalized = normalize_path(Path::new(r"C:\foo\bar\baz.txt"));
    assert_eq!(normalized, "C:/foo/bar/baz.txt");
}

#[cfg(windows)]
#[test]
fn normalize_verbatim_disk_paths_drop_unc_prefix() {
    let normalized = normalize_path(Path::new(r"\\?\C:\foo\bar"));
    assert_eq!(normalized, "C:/foo/bar");
}

#[cfg(windows)]
#[test]
fn normalize_verbatim_unc_paths_match_standard_unc_rendering() {
    let normalized = normalize_path(Path::new(r"\\?\UNC\server\share\dir"));
    assert_eq!(normalized, "//server/share/dir");
}

#[test]
fn normalize_windows_drive_roots_include_trailing_separator() {
    let normalized = normalize_path(Path::new(r"C:\"));
    assert_eq!(normalized, "C:/");
}

#[test]
fn normalize_unc_like_paths_retains_server_share_structure() {
    let normalized = normalize_path(Path::new(r"\\server\share\dir\file"));
    assert_eq!(normalized, "//server/share/dir/file");
}

#[test]
fn message_source_from_accepts_explicit_location() {
    let caller = std::panic::Location::caller();
    let location = message_source_from!(caller);

    assert_eq!(location.line(), caller.line());
    assert!(
        location.path().starts_with(TESTS_DIR),
        "expected {} to start with {}",
        location.path(),
        TESTS_DIR
    );
}

#[test]
fn tracked_message_source_propagates_caller_location() {
    let expected_line = line!() + 1;
    let location = tracked_source();

    assert_eq!(location.line(), expected_line);
    assert!(
        location.path().starts_with(TESTS_DIR),
        "expected {} to start with {}",
        location.path(),
        TESTS_DIR
    );

    let helper_location = untracked_source();
    assert_ne!(helper_location.line(), expected_line);
    assert_eq!(helper_location.path(), location.path());
}

#[test]
fn message_is_hashable() {
    let mut dedupe = HashSet::new();
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    assert!(dedupe.insert(message.clone()));
    assert!(!dedupe.insert(message));
}

#[test]
fn message_clone_preserves_rendering_and_metadata() {
    let original = Message::error(12, "protocol error")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let cloned = original.clone();

    assert_eq!(original, cloned);
    assert_eq!(cloned.to_string(), original.to_string());
    assert_eq!(cloned.code(), Some(12));
    assert_eq!(cloned.role(), Some(Role::Sender));
}

#[test]
fn render_to_matches_display_output() {
    let message = Message::error(35, "timeout in data send")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let mut rendered = String::new();
    message
        .render_to(&mut rendered)
        .expect("rendering into a string never fails");

    assert_eq!(rendered, message.to_string());
}

#[test]
fn render_to_writer_matches_render_to() {
    let message = Message::warning("soft limit reached")
        .with_role(Role::Daemon)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    message
        .render_to_writer(&mut buffer)
        .expect("writing into a vector never fails");

    assert_eq!(buffer, message.to_string().into_bytes());
}

