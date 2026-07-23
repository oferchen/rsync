use super::common::*;
use super::*;

// upstream: options.c:1762-1766 - a rejected --chmod arg prints
// `Invalid argument passed to --chmod (%s)` (verbatim raw arg) and exits
// RERR_SYNTAX (1). We assert the message and exit code byte-for-byte.
#[test]
fn run_reports_invalid_chmod_specification() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    // A category letter (u/g/o) in the permission half is rejected exactly as
    // upstream chmod.c STATE_2ND_HALF does; rsync has no chmod(1)-style copy
    // form. Also exercise a plainly bogus letter.
    for spec in ["a+q", "g=u", "o=g", "u+g"] {
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(format!("--chmod={spec}")),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(code, 1, "`--chmod={spec}` must exit RERR_SYNTAX");
        assert!(stdout.is_empty());
        let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
        assert!(
            rendered.contains(&format!("Invalid argument passed to --chmod ({spec})")),
            "diagnostic for `{spec}` was: {rendered}"
        );
    }
}

#[test]
fn clap_error_uses_canonical_exit_code_text() {
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(OC_RSYNC),
        OsString::from("--definitely-invalid-option"),
    ]);

    assert_eq!(code, 1);

    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(rendered.contains("syntax or usage error"));
    assert!(
        rendered.contains("--definitely-invalid-option"),
        "diagnostic should include the clap-provided detail"
    );
}

// upstream: options.c:1749-1754 - the 21st alt-dest arg overflows the shared
// `basis_dir[]` array (rsync.h:196 MAX_BASIS_DIRS) and exits RERR_SYNTAX (1)
// with a verbatim `ERROR: at most 20 <opt> args may be specified` message. We
// assert both the exit code and the message end-to-end.
#[test]
fn run_rejects_excess_alt_dest_dirs() {
    let mut args = vec![OsString::from(OC_RSYNC)];
    for index in 0..21 {
        args.push(OsString::from(format!("--link-dest=dir{index}")));
    }
    args.push(OsString::from("src"));
    args.push(OsString::from("dst"));

    let (code, stdout, stderr) = run_with_args(args);

    assert_eq!(code, 1, "21 alt-dest dirs must exit RERR_SYNTAX");
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(
        rendered.contains("ERROR: at most 20 --link-dest args may be specified"),
        "diagnostic was: {rendered}"
    );
}

#[test]
fn clap_value_error_does_not_double_error_prefix() {
    // A clap value-validation failure (here `--block-size`) surfaces through
    // `clap::Error::to_string()`, which prepends a stock `error: ` header. oc
    // wraps the detail with the canonical `syntax or usage error: ` category
    // (upstream `rerr_names`), so an unstripped clap header would render the
    // wording twice as `syntax or usage error: error: ...`. Upstream rsync
    // prints the category exactly once; this guards that parity.
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(OC_RSYNC),
        OsString::from("--block-size=abc"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);

    // RERR_SYNTAX (1) must be preserved; only the wording changes.
    assert_eq!(code, 1);

    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(
        rendered.contains("syntax or usage error"),
        "diagnostic should keep the canonical rerr_names category"
    );
    assert!(
        rendered.contains("invalid --block-size value 'abc'"),
        "diagnostic should include the clap-provided detail"
    );
    assert!(
        !rendered.contains("syntax or usage error: error:"),
        "clap's stock `error: ` header must not double the category prefix"
    );
    assert_contains_client_trailer(&rendered);
}

#[test]
fn run_without_operands_emits_usage_for_oc_rsync() {
    let (code, stdout, stderr) = run_with_args([OsString::from(OC_RSYNC)]);

    assert_eq!(code, 1);

    let stdout_text = String::from_utf8(stdout).expect("stdout utf8");
    assert_eq!(
        stdout_text,
        render_missing_operands_stdout(ProgramName::OcRsync)
    );

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains("syntax or usage error"));
    assert_contains_client_trailer(&stderr_text);
}

#[test]
fn run_without_operands_emits_usage_for_legacy_rsync() {
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC)]);

    assert_eq!(code, 1);

    let stdout_text = String::from_utf8(stdout).expect("stdout utf8");
    assert_eq!(
        stdout_text,
        render_missing_operands_stdout(ProgramName::Rsync)
    );

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains("syntax or usage error"));
    assert_contains_client_trailer(&stderr_text);
}
