#[test]
fn parse_args_passes_no_detach_to_remainder() {
    let args = [OC_RSYNC_D, "--no-detach", "--port=8873"];
    let parsed = crate::daemon::parse_args(args).expect("parse args");
    assert!(
        parsed.remainder.iter().any(|a| a == "--no-detach"),
        "--no-detach should pass through to remainder"
    );
}

#[test]
fn parse_args_passes_detach_to_remainder() {
    let args = [OC_RSYNC_D, "--detach"];
    let parsed = crate::daemon::parse_args(args).expect("parse args");
    assert!(
        parsed.remainder.iter().any(|a| a == "--detach"),
        "--detach should pass through to remainder"
    );
}
