/// The daemon `--bwlimit` is a bare KiB integer: a size suffix and a `:BURST`
/// component are both rejected.
///
/// upstream: options.c:862 `{"bwlimit", 0, POPT_ARG_INT, &daemon_bwlimit, ...}`
/// parses a plain integer. Unlike the client `--bwlimit` (options.c:1714
/// `parse_size_arg(bwlimit_arg, 'K', ...)`) it accepts no size suffix, and
/// upstream has no burst component anywhere in `bwlimit` parsing.
#[test]
fn runtime_options_parse_bwlimit_rejects_suffix_and_burst() {
    for value in ["100K", "100:50", "8M"] {
        let error = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from(value)])
            .expect_err("daemon --bwlimit accepts only a bare KiB integer");
        assert!(
            error.message().to_string().contains("is invalid"),
            "value {value:?} should be rejected: {}",
            error.message()
        );
    }
}
