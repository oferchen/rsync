use super::numbers::{encode_signed_decimal, encode_unsigned_decimal};
use super::source::{
    append_normalized_os_str, canonicalize_or_fallback, compute_workspace_root, normalize_path,
    strip_normalized_workspace_prefix,
};
use super::*;
use crate::{
    message_source, message_source_from, rsync_error, rsync_exit_code, rsync_info, rsync_warning,
    tracked_message_source,
};
use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::{self, IoSlice, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const TESTS_FILE_PATH: &str = "crates/core/src/message/tests.rs";

#[track_caller]
fn tracked_source() -> SourceLocation {
    tracked_message_source!()
}

#[track_caller]
fn untracked_source() -> SourceLocation {
    message_source!()
}

#[track_caller]
fn tracked_rsync_error_macro() -> Message {
    rsync_error!(23, "delta-transfer failure")
}

#[track_caller]
fn tracked_rsync_warning_macro() -> Message {
    rsync_warning!("some files vanished")
}

#[track_caller]
fn tracked_rsync_info_macro() -> Message {
    rsync_info!("negotiation complete")
}

#[track_caller]
fn tracked_rsync_exit_code_macro() -> Message {
    rsync_exit_code!(23).expect("exit code 23 is defined")
}

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
    assert_eq!(path, TESTS_FILE_PATH);
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
    assert!(location.path().ends_with(TESTS_FILE_PATH));
}

#[test]
fn tracked_message_source_propagates_caller_location() {
    let expected_line = line!() + 1;
    let location = tracked_source();

    assert_eq!(location.line(), expected_line);
    assert!(location.path().ends_with(TESTS_FILE_PATH));

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

#[test]
fn with_segments_invokes_closure_with_rendered_bytes() {
    let message = Message::error(35, "timeout in data send")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let expected = message.to_bytes().unwrap();
    let mut collected = Vec::new();

    let value = message.with_segments(false, |segments| {
        for slice in segments {
            collected.extend_from_slice(slice.as_ref());
        }

        0xdead_beefu64
    });

    assert_eq!(value, 0xdead_beefu64);
    assert_eq!(collected, expected);
}

#[test]
fn with_segments_supports_newline_variants() {
    let message = Message::warning("vanished files detected").with_code(24);

    let mut collected = Vec::new();
    message.with_segments(true, |segments| {
        for slice in segments {
            collected.extend_from_slice(slice.as_ref());
        }
    });

    assert_eq!(collected, message.to_line_bytes().unwrap());
}

#[test]
fn with_segments_supports_reentrant_rendering() {
    let message = Message::warning("vanished files detected").with_code(24);
    let expected = message.to_bytes().expect("rendering into Vec never fails");

    message.with_segments(false, |segments| {
        let nested = message
            .to_bytes()
            .expect("rendering inside closure should not panic");
        assert_eq!(nested, expected);

        let flattened = segments.to_vec().expect("collecting segments never fails");
        assert_eq!(flattened, expected);
    });
}

#[test]
fn render_to_writer_with_scratch_matches_fresh_scratch() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut reused = Vec::new();
    message
        .render_to_writer_with_scratch(&mut scratch, &mut reused)
        .expect("writing into a vector never fails");

    let mut baseline = Vec::new();
    message
        .render_to_writer(&mut baseline)
        .expect("writing into a vector never fails");

    assert_eq!(reused, baseline);
}

#[test]
fn scratch_supports_sequential_messages() {
    let mut scratch = MessageScratch::new();
    let mut output = Vec::new();

    rsync_error!(23, "delta-transfer failure")
        .render_line_to_writer_with_scratch(&mut scratch, &mut output)
        .expect("writing into a vector never fails");

    rsync_warning!("some files vanished")
        .with_code(24)
        .render_line_to_writer_with_scratch(&mut scratch, &mut output)
        .expect("writing into a vector never fails");

    let rendered = String::from_utf8(output).expect("messages are UTF-8");
    assert!(rendered.lines().any(|line| line.contains("(code 23)")));
    assert!(rendered.lines().any(|line| line.contains("(code 24)")));
}

#[test]
fn message_segments_iterator_covers_all_bytes() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Receiver)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let collected: Vec<u8> = {
        let segments = message.as_segments(&mut scratch, true);
        segments
            .iter()
            .flat_map(|slice| {
                let bytes: &[u8] = slice.as_ref();
                bytes.iter().copied()
            })
            .collect()
    };

    assert_eq!(collected, message.to_line_bytes().unwrap());
}

#[test]
fn message_segments_iter_bytes_matches_iter() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, false);
    let via_iter: Vec<&[u8]> = segments.iter().map(|slice| slice.as_ref()).collect();
    let via_bytes: Vec<&[u8]> = segments.iter_bytes().collect();

    assert_eq!(via_bytes, via_iter);
}

#[test]
fn message_segments_iter_bytes_supports_double_ended_iteration() {
    let message = Message::warning("vanished").with_code(24);
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, true);
    let forward: Vec<&[u8]> = segments.iter_bytes().collect();
    let reverse: Vec<&[u8]> = segments.iter_bytes().rev().collect();
    let expected_forward: Vec<&[u8]> = segments.iter().map(|slice| slice.as_ref()).collect();
    let expected_reverse: Vec<&[u8]> = expected_forward.iter().rev().copied().collect();

    assert_eq!(forward, expected_forward);
    assert_eq!(reverse, expected_reverse);
}

#[test]
fn message_segments_into_iterator_matches_iter() {
    let message = Message::error(12, "example failure")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, true);
    let via_method: Vec<usize> = segments.iter().map(|slice| slice.len()).collect();
    let via_into: Vec<usize> = (&segments).into_iter().map(|slice| slice.len()).collect();

    assert_eq!(via_method, via_into);
}

#[test]
fn message_segments_mut_iterator_covers_all_bytes() {
    let message = Message::error(24, "partial transfer").with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let mut segments = message.as_segments(&mut scratch, false);
    let mut total_len = 0;

    for slice in &mut segments {
        let bytes: &[u8] = slice.as_ref();
        total_len += bytes.len();
    }

    assert_eq!(total_len, message.to_bytes().unwrap().len());
}

#[test]
fn message_segments_extend_vec_appends_bytes() {
    let message = Message::error(12, "example failure")
        .with_role(Role::Server)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, false);
    let mut buffer = b"prefix: ".to_vec();
    let prefix_len = buffer.len();
    let appended = segments
        .extend_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");

    assert_eq!(&buffer[..prefix_len], b"prefix: ");
    assert_eq!(
        &buffer[prefix_len..],
        message.to_bytes().unwrap().as_slice()
    );
    assert_eq!(appended, message.to_bytes().unwrap().len());
}

#[test]
fn message_segments_extend_vec_noop_for_empty_segments() {
    let segments = MessageSegments {
        segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
        count: 0,
        total_len: 0,
    };

    let mut buffer = b"static prefix".to_vec();
    let expected = buffer.clone();
    let capacity = buffer.capacity();

    let appended = segments
        .extend_vec(&mut buffer)
        .expect("empty segments should not alter the buffer");

    assert_eq!(appended, 0);
    assert_eq!(buffer, expected);
    assert_eq!(buffer.capacity(), capacity);
}

#[test]
fn message_segments_try_extend_vec_appends_bytes() {
    let message = Message::error(12, "example failure")
        .with_role(Role::Server)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, false);
    let mut buffer = b"prefix: ".to_vec();
    let prefix_len = buffer.len();
    let appended = segments
        .try_extend_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");

    assert_eq!(&buffer[..prefix_len], b"prefix: ");
    assert_eq!(
        &buffer[prefix_len..],
        message.to_bytes().unwrap().as_slice()
    );
    assert_eq!(appended, message.to_bytes().unwrap().len());
}

#[test]
fn message_segments_try_extend_vec_noop_for_empty_segments() {
    let segments = MessageSegments {
        segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
        count: 0,
        total_len: 0,
    };

    let mut buffer = b"static prefix".to_vec();
    let expected = buffer.clone();
    let capacity = buffer.capacity();

    let appended = segments
        .try_extend_vec(&mut buffer)
        .expect("empty segments should not alter the buffer");

    assert_eq!(appended, 0);
    assert_eq!(buffer, expected);
    assert_eq!(buffer.capacity(), capacity);
}

#[test]
fn message_segments_copy_to_slice_copies_exact_bytes() {
    let message = Message::error(12, "example failure")
        .with_role(Role::Server)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, true);
    let mut buffer = vec![0u8; segments.len()];
    let copied = segments
        .copy_to_slice(&mut buffer)
        .expect("buffer is large enough");

    assert_eq!(copied, segments.len());
    assert_eq!(buffer, message.to_line_bytes().unwrap());
}

#[test]
fn message_segments_copy_to_slice_reports_required_length() {
    let message = Message::warning("vanished").with_code(24);
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);
    let mut buffer = vec![0u8; segments.len().saturating_sub(1)];

    let err = segments
        .copy_to_slice(&mut buffer)
        .expect_err("buffer is intentionally undersized");

    assert_eq!(err.required(), segments.len());
    assert_eq!(err.provided(), buffer.len());
    assert_eq!(err.missing(), segments.len() - buffer.len());
}

#[test]
fn message_segments_copy_to_slice_accepts_empty_inputs() {
    let segments = MessageSegments {
        segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
        count: 0,
        total_len: 0,
    };

    let mut buffer = [0u8; 0];
    let copied = segments
        .copy_to_slice(&mut buffer)
        .expect("empty segments succeed for empty buffers");

    assert_eq!(copied, 0);
}

#[test]
fn message_copy_to_slice_error_converts_into_io_error() {
    let message = Message::info("ready");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);

    let mut undersized = vec![0u8; segments.len().saturating_sub(1)];
    let err = segments
        .copy_to_slice(&mut undersized)
        .expect_err("buffer is intentionally undersized");
    let io_err: io::Error = err.into();

    assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    let display = io_err.to_string();
    assert_eq!(display, err.to_string());

    let inner = io_err
        .into_inner()
        .expect("conversion retains source error");
    let recovered = inner
        .downcast::<CopyToSliceError>()
        .expect("inner error matches original type");
    assert_eq!(*recovered, err);
}

#[test]
fn message_segments_is_empty_accounts_for_zero_length_segments() {
    let mut scratch = MessageScratch::new();
    let message = Message::info("ready");
    let populated = message.as_segments(&mut scratch, false);
    assert!(!populated.is_empty());

    let empty = MessageSegments {
        segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
        count: 1,
        total_len: 0,
    };

    assert!(empty.is_empty());
}

#[test]
fn message_segments_to_vec_collects_bytes() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Receiver)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, false);
    let collected = segments
        .to_vec()
        .expect("allocating the rendered message succeeds");

    assert_eq!(collected, message.to_bytes().unwrap());
}

#[test]
fn message_segments_to_vec_respects_newline_flag() {
    let message = Message::warning("vanished file").with_code(24);
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, true);
    let collected = segments
        .to_vec()
        .expect("allocating the rendered message succeeds");

    assert_eq!(collected, message.to_line_bytes().unwrap());
}

#[test]
fn render_line_to_appends_newline() {
    let message = Message::warning("soft limit reached");

    let mut rendered = String::new();
    message
        .render_line_to(&mut rendered)
        .expect("rendering into a string never fails");

    assert_eq!(rendered, format!("{}\n", message));
}

#[test]
fn render_to_with_scratch_matches_standard_rendering() {
    let message = Message::warning("soft limit reached")
        .with_code(24)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut reused = String::new();
    message
        .render_to_with_scratch(&mut scratch, &mut reused)
        .expect("rendering into a string never fails");

    let mut baseline = String::new();
    message
        .render_to(&mut baseline)
        .expect("rendering into a string never fails");

    assert_eq!(reused, baseline);
}

#[test]
fn render_to_writer_matches_render_to_for_negative_codes() {
    let message = Message::error(-35, "timeout in data send")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    message
        .render_to_writer(&mut buffer)
        .expect("writing into a vector never fails");

    assert_eq!(buffer, message.to_string().into_bytes());
}

#[test]
fn segments_match_rendered_output() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, true);

    let mut aggregated = Vec::new();
    for slice in segments.as_slices() {
        aggregated.extend_from_slice(slice.as_ref());
    }

    assert_eq!(aggregated, message.to_line_bytes().unwrap());
    assert_eq!(segments.len(), aggregated.len());
    assert!(segments.segment_count() > 1);
}

#[test]
fn segments_handle_messages_without_optional_fields() {
    let message = Message::info("protocol handshake complete");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);

    let mut combined = Vec::new();
    for slice in segments.as_slices() {
        combined.extend_from_slice(slice.as_ref());
    }

    assert_eq!(combined, message.to_bytes().unwrap());
    assert_eq!(segments.segment_count(), segments.as_slices().len());
    assert!(!segments.is_empty());
}

#[test]
fn render_line_to_writer_appends_newline() {
    let message = Message::info("protocol handshake complete");

    let mut buffer = Vec::new();
    message
        .render_line_to_writer(&mut buffer)
        .expect("writing into a vector never fails");

    assert_eq!(buffer, format!("{}\n", message).into_bytes());
}

#[test]
fn to_bytes_matches_display_output() {
    let message = Message::error(11, "read failure")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let rendered = message.to_bytes().expect("Vec<u8> writes are infallible");
    let expected = message.to_string().into_bytes();

    assert_eq!(rendered, expected);
}

#[test]
fn byte_len_matches_rendered_length() {
    let message = Message::error(35, "timeout waiting for daemon connection")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let rendered = message.to_bytes().expect("Vec<u8> writes are infallible");

    assert_eq!(message.byte_len(), rendered.len());
}

#[test]
fn to_line_bytes_appends_newline() {
    let message = Message::warning("vanished")
        .with_code(24)
        .with_source(message_source!());

    let rendered = message
        .to_line_bytes()
        .expect("Vec<u8> writes are infallible");
    let expected = {
        let mut buf = message.to_string().into_bytes();
        buf.push(b'\n');
        buf
    };

    assert_eq!(rendered, expected);
}

#[test]
fn line_byte_len_matches_rendered_length() {
    let message = Message::warning("some files vanished")
        .with_code(24)
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let rendered = message
        .to_line_bytes()
        .expect("Vec<u8> writes are infallible");

    assert_eq!(message.line_byte_len(), rendered.len());
}

#[test]
fn append_to_vec_matches_to_bytes() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    let appended = message
        .append_to_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");

    assert_eq!(buffer, message.to_bytes().unwrap());
    assert_eq!(appended, buffer.len());
}

#[test]
fn append_line_to_vec_matches_to_line_bytes() {
    let message = Message::warning("vanished")
        .with_code(24)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    let appended = message
        .append_line_to_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");

    assert_eq!(buffer, message.to_line_bytes().unwrap());
    assert_eq!(appended, buffer.len());
}

#[test]
fn append_with_scratch_accumulates_messages() {
    let message = Message::error(11, "read failure")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut buffer = Vec::new();
    let appended = message
        .append_to_vec_with_scratch(&mut scratch, &mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");
    let first_len = buffer.len();
    let without_newline = message.to_bytes().unwrap();
    assert_eq!(appended, without_newline.len());

    let appended_line = message
        .append_line_to_vec_with_scratch(&mut scratch, &mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");
    let with_newline = message
        .to_line_bytes()
        .expect("Vec<u8> writes are infallible");
    assert_eq!(appended_line, with_newline.len());

    assert_eq!(&buffer[..first_len], without_newline.as_slice());
    assert_eq!(&buffer[first_len..], with_newline.as_slice());
}

#[test]
fn to_bytes_with_scratch_matches_standard_rendering() {
    let message = Message::info("protocol handshake complete").with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let reused = message
        .to_line_bytes_with_scratch(&mut scratch)
        .expect("Vec<u8> writes are infallible");

    let baseline = message
        .to_line_bytes()
        .expect("Vec<u8> writes are infallible");

    assert_eq!(reused, baseline);
}

struct FailingWriter;

impl io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("sink error"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn render_to_writer_propagates_io_error() {
    let mut writer = FailingWriter;
    let message = Message::info("protocol handshake complete");

    let err = message
        .render_to_writer(&mut writer)
        .expect_err("writer error should propagate");

    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(err.to_string(), "sink error");
}

struct NewlineFailingWriter;

impl io::Write for NewlineFailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf == b"\n" {
            Err(io::Error::other("newline sink error"))
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn render_line_to_writer_propagates_newline_error() {
    let mut writer = NewlineFailingWriter;
    let message = Message::warning("soft limit reached");

    let err = message
        .render_line_to_writer(&mut writer)
        .expect_err("newline error should propagate");

    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(err.to_string(), "newline sink error");
}

#[derive(Default)]
struct InterruptingVectoredWriter {
    buffer: Vec<u8>,
    remaining_interrupts: usize,
}

impl InterruptingVectoredWriter {
    fn new(interruptions: usize) -> Self {
        Self {
            remaining_interrupts: interruptions,
            ..Self::default()
        }
    }
}

impl io::Write for InterruptingVectoredWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        if self.remaining_interrupts > 0 {
            self.remaining_interrupts -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }

        let mut written = 0usize;
        for slice in bufs {
            self.buffer.extend_from_slice(slice.as_ref());
            written += slice.len();
        }

        Ok(written)
    }
}

#[test]
fn render_to_writer_retries_after_interrupted_vectored_write() {
    let message = Message::info("protocol negotiation complete");
    let mut writer = InterruptingVectoredWriter::new(1);

    message
        .render_to_writer(&mut writer)
        .expect("interrupted writes should be retried");

    assert_eq!(writer.remaining_interrupts, 0);
    assert_eq!(writer.buffer, message.to_string().into_bytes());
}

#[test]
fn render_to_writer_uses_thread_local_scratch_per_thread() {
    use std::sync::{Arc, Barrier};
    use std::thread;

    let message = Message::error(42, "per-thread scratch")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let barrier = Arc::new(Barrier::new(4));

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let message = message.clone();

            thread::spawn(move || {
                barrier.wait();
                let expected = message.to_string().into_bytes();

                for _ in 0..64 {
                    let mut buffer = Vec::new();
                    message
                        .render_to_writer(&mut buffer)
                        .expect("Vec<u8> writes are infallible");

                    assert_eq!(buffer, expected);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread panicked");
    }
}

#[test]
fn render_to_writer_coalesces_segments_for_vectored_writer() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(untracked_source());

    let expected = message.to_string();

    let mut writer = RecordingWriter::new();
    message
        .render_to_writer(&mut writer)
        .expect("vectored write succeeds");

    assert_eq!(writer.vectored_calls, 1, "single vectored write expected");
    assert_eq!(
        writer.write_calls, 0,
        "sequential fallback should be unused"
    );
    assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
}

#[test]
fn render_to_writer_skips_vectored_when_writer_does_not_support_it() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Receiver)
        .with_source(untracked_source());

    let expected = message.to_string();

    let mut writer = RecordingWriter::without_vectored();
    message
        .render_to_writer(&mut writer)
        .expect("sequential write succeeds");

    assert_eq!(writer.vectored_calls, 0, "vectored writes must be skipped");
    assert!(
        writer.write_calls > 0,
        "sequential path should handle the message"
    );
    assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
}

#[test]
fn render_to_writer_falls_back_when_vectored_partial() {
    let message = Message::error(30, "timeout in data send/receive")
        .with_role(Role::Receiver)
        .with_source(untracked_source());

    let expected = message.to_string();

    let mut writer = RecordingWriter::with_vectored_limit(5);
    message
        .render_to_writer(&mut writer)
        .expect("fallback write succeeds");

    assert!(
        writer.vectored_calls >= 1,
        "vectored path should be attempted at least once"
    );
    assert!(
        writer.write_calls > 0,
        "sequential fallback must finish the message"
    );
    assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
}

#[test]
fn segments_as_ref_exposes_slice_view() {
    let mut scratch = MessageScratch::new();
    let message = Message::error(35, "timeout waiting for daemon connection")
        .with_role(Role::Sender)
        .with_source(untracked_source());

    let segments = message.as_segments(&mut scratch, false);
    let slices = segments.as_ref();

    assert_eq!(slices.len(), segments.segment_count());

    let flattened: Vec<u8> = slices
        .iter()
        .flat_map(|slice| {
            let bytes: &[u8] = slice.as_ref();
            bytes.iter().copied()
        })
        .collect();

    assert_eq!(flattened, message.to_bytes().unwrap());
}

#[test]
fn segments_into_iter_collects_bytes() {
    let mut scratch = MessageScratch::new();
    let message = Message::warning("some files vanished")
        .with_code(24)
        .with_source(untracked_source());

    let segments = message.as_segments(&mut scratch, true);
    let mut flattened = Vec::new();

    for slice in segments.clone() {
        let bytes: &[u8] = slice.as_ref();
        flattened.extend_from_slice(bytes);
    }

    assert_eq!(flattened, message.to_line_bytes().unwrap());
}

#[test]
fn segments_into_iter_respects_segment_count() {
    let mut scratch = MessageScratch::new();
    let message = Message::info("protocol negotiation complete");

    let segments = message.as_segments(&mut scratch, false);
    let iter = segments.clone().into_iter();

    assert_eq!(iter.count(), segments.segment_count());
}

struct RecordingWriter {
    buffer: Vec<u8>,
    vectored_calls: usize,
    write_calls: usize,
    vectored_limit: Option<usize>,
    supports_vectored: bool,
}

impl RecordingWriter {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            vectored_calls: 0,
            write_calls: 0,
            vectored_limit: None,
            supports_vectored: true,
        }
    }

    fn with_vectored_limit(limit: usize) -> Self {
        let mut writer = Self::new();
        writer.vectored_limit = Some(limit);
        writer
    }

    fn without_vectored() -> Self {
        let mut writer = Self::new();
        writer.supports_vectored = false;
        writer
    }
}

impl IoWrite for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        if !self.supports_vectored {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored writes unsupported",
            ));
        }
        self.vectored_calls += 1;

        let mut to_write: usize = bufs.iter().map(|slice| slice.len()).sum();
        if let Some(limit) = self.vectored_limit {
            let capped = to_write.min(limit);
            self.vectored_limit = Some(limit.saturating_sub(capped));
            to_write = capped;

            if to_write == 0 {
                self.supports_vectored = false;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "vectored limit reached",
                ));
            }
        }

        let mut remaining = to_write;
        for slice in bufs {
            if remaining == 0 {
                break;
            }

            let data: &[u8] = slice.as_ref();
            let portion = data.len().min(remaining);
            self.buffer.extend_from_slice(&data[..portion]);
            remaining -= portion;
        }

        Ok(to_write)
    }
}

#[test]
fn severity_as_str_matches_expected_labels() {
    assert_eq!(Severity::Info.as_str(), "info");
    assert_eq!(Severity::Warning.as_str(), "warning");
    assert_eq!(Severity::Error.as_str(), "error");
}

#[test]
fn severity_prefix_matches_expected_strings() {
    assert_eq!(Severity::Info.prefix(), "rsync info: ");
    assert_eq!(Severity::Warning.prefix(), "rsync warning: ");
    assert_eq!(Severity::Error.prefix(), "rsync error: ");
}

#[test]
fn severity_display_matches_as_str() {
    assert_eq!(Severity::Info.to_string(), "info");
    assert_eq!(Severity::Warning.to_string(), "warning");
    assert_eq!(Severity::Error.to_string(), "error");
}

#[test]
fn severity_predicates_match_variants() {
    assert!(Severity::Info.is_info());
    assert!(!Severity::Info.is_warning());
    assert!(!Severity::Info.is_error());

    assert!(Severity::Warning.is_warning());
    assert!(!Severity::Warning.is_info());
    assert!(!Severity::Warning.is_error());

    assert!(Severity::Error.is_error());
    assert!(!Severity::Error.is_info());
    assert!(!Severity::Error.is_warning());
}

#[test]
fn severity_from_str_parses_known_labels() {
    assert_eq!(Severity::from_str("info"), Ok(Severity::Info));
    assert_eq!(Severity::from_str("warning"), Ok(Severity::Warning));
    assert_eq!(Severity::from_str("error"), Ok(Severity::Error));
}

#[test]
fn severity_from_str_rejects_unknown_labels() {
    assert!(Severity::from_str("verbose").is_err());
}

#[test]
fn role_as_str_matches_expected_labels() {
    assert_eq!(Role::Sender.as_str(), "sender");
    assert_eq!(Role::Receiver.as_str(), "receiver");
    assert_eq!(Role::Generator.as_str(), "generator");
    assert_eq!(Role::Server.as_str(), "server");
    assert_eq!(Role::Client.as_str(), "client");
    assert_eq!(Role::Daemon.as_str(), "daemon");
}

#[test]
fn role_display_matches_as_str() {
    assert_eq!(Role::Sender.to_string(), "sender");
    assert_eq!(Role::Daemon.to_string(), "daemon");
}

#[test]
fn role_from_str_parses_known_labels() {
    assert_eq!(Role::from_str("sender"), Ok(Role::Sender));
    assert_eq!(Role::from_str("receiver"), Ok(Role::Receiver));
    assert_eq!(Role::from_str("generator"), Ok(Role::Generator));
    assert_eq!(Role::from_str("server"), Ok(Role::Server));
    assert_eq!(Role::from_str("client"), Ok(Role::Client));
    assert_eq!(Role::from_str("daemon"), Ok(Role::Daemon));
}

#[test]
fn role_from_str_rejects_unknown_labels() {
    assert!(Role::from_str("observer").is_err());
}

#[test]
fn role_all_lists_every_variant_once_in_canonical_order() {
    assert_eq!(
        Role::ALL,
        [
            Role::Sender,
            Role::Receiver,
            Role::Generator,
            Role::Server,
            Role::Client,
            Role::Daemon,
        ]
    );

    for (index, outer) in Role::ALL.iter().enumerate() {
        for inner in Role::ALL.iter().skip(index + 1) {
            assert_ne!(outer, inner, "Role::ALL must not contain duplicates");
        }
    }
}

#[test]
fn encode_unsigned_decimal_formats_expected_values() {
    let mut buf = [0u8; 8];
    assert_eq!(encode_unsigned_decimal(0, &mut buf), "0");
    assert_eq!(encode_unsigned_decimal(42, &mut buf), "42");
    assert_eq!(encode_unsigned_decimal(12_345_678, &mut buf), "12345678");
}

#[test]
fn encode_signed_decimal_handles_positive_and_negative_values() {
    let mut buf = [0u8; 12];
    assert_eq!(encode_signed_decimal(0, &mut buf), "0");
    assert_eq!(encode_signed_decimal(123, &mut buf), "123");
    assert_eq!(encode_signed_decimal(-456, &mut buf), "-456");
}

#[test]
fn encode_signed_decimal_formats_i64_minimum_value() {
    let mut buf = [0u8; 32];
    assert_eq!(
        encode_signed_decimal(i64::MIN, &mut buf),
        "-9223372036854775808"
    );
}

#[test]
fn render_to_writer_formats_minimum_exit_code() {
    let message = Message::error(i32::MIN, "integrity check failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    message
        .render_to_writer(&mut buffer)
        .expect("rendering into a vector never fails");

    let rendered = String::from_utf8(buffer).expect("message renders as UTF-8");
    assert!(rendered.contains("(code -2147483648)"));
}

#[test]
fn rsync_error_macro_attaches_source_and_code() {
    let message = rsync_error!(23, "delta-transfer failure");

    assert_eq!(message.severity(), Severity::Error);
    assert_eq!(message.code(), Some(23));
    let source = message.source().expect("macro records source location");
    assert!(source.path().ends_with(TESTS_FILE_PATH));
}

#[test]
fn rsync_error_macro_honors_track_caller() {
    let expected_line = line!() + 1;
    let message = tracked_rsync_error_macro();
    let source = message.source().expect("macro records source location");

    assert_eq!(source.line(), expected_line);
    assert!(source.path().ends_with(TESTS_FILE_PATH));
}

#[test]
fn rsync_warning_macro_supports_format_arguments() {
    let message = rsync_warning!("vanished {count} files", count = 2).with_code(24);

    assert_eq!(message.severity(), Severity::Warning);
    assert_eq!(message.code(), Some(24));
    assert_eq!(message.text(), "vanished 2 files");
}

#[test]
fn rsync_warning_macro_honors_track_caller() {
    let expected_line = line!() + 1;
    let message = tracked_rsync_warning_macro();
    let source = message.source().expect("macro records source location");

    assert_eq!(source.line(), expected_line);
    assert!(source.path().ends_with(TESTS_FILE_PATH));
}

#[test]
fn rsync_info_macro_attaches_source() {
    let message = rsync_info!("protocol {version} negotiated", version = 32);

    assert_eq!(message.severity(), Severity::Info);
    assert_eq!(message.code(), None);
    assert_eq!(message.text(), "protocol 32 negotiated");
    assert!(message.source().is_some());
}

#[test]
fn rsync_info_macro_honors_track_caller() {
    let expected_line = line!() + 1;
    let message = tracked_rsync_info_macro();
    let source = message.source().expect("macro records source location");

    assert_eq!(source.line(), expected_line);
    assert!(source.path().ends_with(TESTS_FILE_PATH));
}

#[test]
fn rsync_exit_code_macro_returns_message_for_known_code() {
    let message = rsync_exit_code!(23).expect("exit code 23 is defined");

    assert_eq!(message.severity(), Severity::Error);
    assert_eq!(message.code(), Some(23));
    assert!(
        message
            .text()
            .contains("some files/attrs were not transferred")
    );
    assert!(message.source().is_some());
}

#[test]
fn rsync_exit_code_macro_returns_none_for_unknown_code() {
    assert!(rsync_exit_code!(7).is_none());
}

#[test]
fn rsync_exit_code_macro_honors_track_caller() {
    let expected_line = line!() + 1;
    let message = tracked_rsync_exit_code_macro();
    let source = message.source().expect("macro records source location");

    assert_eq!(source.line(), expected_line);
    assert!(source.path().ends_with(TESTS_FILE_PATH));
}

#[test]
fn append_normalized_os_str_rewrites_backslashes() {
    let mut rendered = String::from("prefix/");
    append_normalized_os_str(&mut rendered, OsStr::new(r"dir\file.txt"));

    assert_eq!(rendered, "prefix/dir/file.txt");
}

#[test]
fn append_normalized_os_str_preserves_existing_forward_slashes() {
    let mut rendered = String::new();
    append_normalized_os_str(&mut rendered, OsStr::new("dir/sub"));

    assert_eq!(rendered, "dir/sub");
}

#[test]
fn append_normalized_os_str_handles_unc_prefixes() {
    let mut rendered = String::new();
    append_normalized_os_str(&mut rendered, OsStr::new(r"\\server\share\path"));

    assert_eq!(rendered, "//server/share/path");
}

#[test]
fn append_normalized_os_str_preserves_trailing_backslash() {
    let mut rendered = String::new();
    append_normalized_os_str(&mut rendered, OsStr::new(r#"C:\path\to\dir\"#));

    assert_eq!(rendered, "C:/path/to/dir/");
}

#[derive(Default)]
struct TrackingWriter {
    written: Vec<u8>,
    vectored_calls: usize,
    unsupported_once: bool,
    always_unsupported: bool,
    vectored_limit: Option<usize>,
}

impl TrackingWriter {
    fn with_unsupported_once() -> Self {
        Self {
            unsupported_once: true,
            ..Self::default()
        }
    }

    fn with_always_unsupported() -> Self {
        Self {
            always_unsupported: true,
            ..Self::default()
        }
    }

    fn with_vectored_limit(limit: usize) -> Self {
        Self {
            vectored_limit: Some(limit),
            ..Self::default()
        }
    }
}

impl io::Write for TrackingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;

        if self.unsupported_once {
            self.unsupported_once = false;
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no vectored support",
            ));
        }

        if self.always_unsupported {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no vectored support",
            ));
        }

        let mut limit = self.vectored_limit.unwrap_or(usize::MAX);
        let mut total = 0usize;
        for buf in bufs {
            if limit == 0 {
                break;
            }

            let slice: &[u8] = buf.as_ref();
            let take = slice.len().min(limit);
            self.written.extend_from_slice(&slice[..take]);
            total += take;
            limit -= take;

            if take < slice.len() {
                break;
            }
        }

        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct PartialThenUnsupportedWriter {
    written: Vec<u8>,
    vectored_calls: usize,
    fallback_writes: usize,
    limit: usize,
}

impl PartialThenUnsupportedWriter {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
}

impl io::Write for PartialThenUnsupportedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.fallback_writes += 1;
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;

        if self.vectored_calls == 1 {
            let mut limit = self.limit;
            let mut total = 0usize;

            for buf in bufs {
                if limit == 0 {
                    break;
                }

                let slice: &[u8] = buf.as_ref();
                let take = slice.len().min(limit);
                self.written.extend_from_slice(&slice[..take]);
                total += take;
                limit -= take;

                if take < slice.len() {
                    break;
                }
            }

            return Ok(total);
        }

        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "vectored disabled after first call",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct OverreportingWriter {
    buffer: Vec<u8>,
}

impl io::Write for OverreportingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut total = 0usize;

        for buf in bufs {
            let slice: &[u8] = buf.as_ref();
            self.buffer.extend_from_slice(slice);
            total += slice.len();
        }

        Ok(total.saturating_add(1))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct ZeroProgressWriter {
    write_calls: usize,
}

impl io::Write for ZeroProgressWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        Ok(buf.len())
    }

    fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct LeadingEmptyAwareWriter {
    buffer: Vec<u8>,
    vectored_calls: usize,
    write_calls: usize,
}

impl io::Write for LeadingEmptyAwareWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;

        if bufs.first().is_some_and(|slice| {
            let bytes: &[u8] = slice.as_ref();
            bytes.is_empty()
        }) {
            return Ok(0);
        }

        let mut total = 0;
        for slice in bufs {
            let bytes: &[u8] = slice.as_ref();
            self.buffer.extend_from_slice(bytes);
            total += bytes.len();
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn segments_write_to_prefers_vectored_io() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = TrackingWriter::default();

    {
        let segments = message.as_segments(&mut scratch, true);
        segments
            .write_to(&mut writer)
            .expect("writing into a vector never fails");
    }

    assert_eq!(writer.written, message.to_line_bytes().unwrap());
    assert!(writer.vectored_calls >= 1);
}

#[test]
fn segments_write_to_skips_vectored_for_single_segment() {
    let message = Message::info("");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);

    assert_eq!(segments.segment_count(), 1);

    let mut writer = RecordingWriter::new();
    segments
        .write_to(&mut writer)
        .expect("single-segment writes succeed");

    assert_eq!(writer.vectored_calls, 0, "vectored path should be skipped");
    assert_eq!(writer.write_calls, 1, "single write_all call expected");
    assert_eq!(writer.buffer, message.to_bytes().unwrap());
}

#[test]
fn segments_write_to_falls_back_after_unsupported_vectored_call() {
    let message = Message::error(30, "timeout in data send/receive")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = TrackingWriter::with_unsupported_once();

    {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect("sequential fallback should succeed");
    }

    assert_eq!(writer.written, message.to_bytes().unwrap());
    assert_eq!(writer.vectored_calls, 1);
}

#[test]
fn segments_write_to_skips_leading_empty_slices_before_vectored_write() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut segments = message.as_segments(&mut scratch, false);

    let original_count = segments.count;
    assert!(original_count < MAX_MESSAGE_SEGMENTS);

    for index in (0..original_count).rev() {
        segments.segments[index + 1] = segments.segments[index];
    }
    segments.segments[0] = IoSlice::new(&[]);
    segments.count = original_count + 1;
    // The total length remains unchanged because the new segment is empty.

    let mut writer = LeadingEmptyAwareWriter::default();
    segments
        .write_to(&mut writer)
        .expect("leading empty slices should not trigger write_zero errors");

    assert_eq!(writer.buffer, message.to_bytes().unwrap());
    assert_eq!(
        writer.vectored_calls, 1,
        "vectored path should succeed once"
    );
    assert_eq!(writer.write_calls, 0, "no sequential fallback expected");
}

#[test]
fn segments_write_to_handles_persistent_unsupported_vectored_calls() {
    let message = Message::error(124, "remote shell failed")
        .with_role(Role::Client)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = TrackingWriter::with_always_unsupported();

    {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect("sequential fallback should succeed");
    }

    assert_eq!(writer.written, message.to_bytes().unwrap());
    assert_eq!(writer.vectored_calls, 1);
}

#[test]
fn segments_write_to_errors_when_total_len_underreports_written_bytes() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut segments = message.as_segments(&mut scratch, false);
    assert!(
        segments.segment_count() > 1,
        "test requires multiple segments"
    );
    assert!(!segments.is_empty(), "message must contain bytes");

    segments.total_len = segments
        .total_len
        .checked_sub(1)
        .expect("total length should exceed one byte");

    let mut writer = TrackingWriter::with_always_unsupported();

    let err = segments
        .write_to(&mut writer)
        .expect_err("length mismatch must produce an error");

    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn segments_write_to_retries_after_partial_vectored_write() {
    let message = Message::error(35, "protocol generator aborted")
        .with_role(Role::Generator)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = TrackingWriter::with_vectored_limit(8);

    {
        let segments = message.as_segments(&mut scratch, true);
        segments
            .write_to(&mut writer)
            .expect("partial vectored writes should succeed");
    }

    assert_eq!(writer.written, message.to_line_bytes().unwrap());
    assert!(writer.vectored_calls >= 2);
}

#[test]
fn segments_write_to_handles_partial_then_unsupported_vectored_call() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = PartialThenUnsupportedWriter::new(8);

    {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect("sequential fallback should succeed after partial vectored writes");
    }

    assert_eq!(writer.written, message.to_bytes().unwrap());
    assert_eq!(writer.vectored_calls, 2);
    assert!(writer.fallback_writes >= 1);
}

#[test]
fn segments_write_to_handles_cross_slice_progress_before_unsupported_vectored_call() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = PartialThenUnsupportedWriter::new(18);

    {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect("sequential fallback should succeed after cross-slice progress");
    }

    assert_eq!(writer.written, message.to_bytes().unwrap());
    assert_eq!(writer.vectored_calls, 2);
    assert!(writer.fallback_writes >= 1);
}

#[test]
fn segments_write_to_errors_when_vectored_makes_no_progress() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = ZeroProgressWriter::default();

    let err = {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect_err("zero-length vectored write must error")
    };

    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    assert_eq!(writer.write_calls, 0, "sequential writes should not run");
}

#[test]
fn segments_write_to_errors_when_writer_overreports_progress() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = OverreportingWriter::default();

    let err = {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect_err("overreporting writer must trigger an error")
    };

    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(writer.buffer, message.to_bytes().unwrap());
}
