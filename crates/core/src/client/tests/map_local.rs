use super::prelude::*;


#[test]
fn map_local_copy_error_reports_delete_limit() {
    let mapped = map_local_copy_error(LocalCopyError::delete_limit_exceeded(2));
    assert_eq!(mapped.exit_code(), MAX_DELETE_EXIT_CODE);
    let rendered = mapped.message().to_string();
    assert!(
        rendered.contains("Deletions stopped due to --max-delete limit (2 entries skipped)"),
        "unexpected diagnostic: {rendered}"
    );
}

