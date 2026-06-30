#[cfg(test)]
mod daemon_chmod_spec_tests {
    use super::parse_one_chmod_spec;

    #[test]
    fn parse_one_chmod_spec_returns_none_for_unset_directive() {
        let result = parse_one_chmod_spec("incoming chmod", None).expect("ok");
        assert!(result.is_none());
    }

    #[test]
    fn parse_one_chmod_spec_accepts_numeric_class_action_form() {
        // upstream parse_chmod() accepts bare octal (`644`), prefix-letter
        // (`F600`), class-action-perms (`u+x`), and combined forms.
        for spec in ["644", "F600", "u+x", "Du+x,Fg-r,Fo-r"] {
            let parsed = parse_one_chmod_spec("incoming chmod", Some(spec))
                .unwrap_or_else(|err| panic!("spec '{spec}' must parse: {err}"));
            assert!(parsed.is_some(), "spec '{spec}' produced no clauses");
        }
    }

    #[test]
    fn parse_one_chmod_spec_surfaces_directive_name_on_error() {
        let err = parse_one_chmod_spec("outgoing chmod", Some("bogus"))
            .expect_err("malformed spec must error");
        assert!(
            err.contains("outgoing chmod"),
            "error '{err}' must name the offending directive",
        );
        assert!(
            err.contains("bogus"),
            "error '{err}' must include the offending spec text",
        );
    }
}

#[cfg(test)]
mod iconv_charset_converter_tests {
    use super::resolve_module_charset_converter;

    #[test]
    fn iconv_charset_returns_none_for_missing_directive() {
        assert!(resolve_module_charset_converter(None).is_none());
    }

    #[test]
    fn iconv_charset_returns_none_for_empty_directive() {
        assert!(resolve_module_charset_converter(Some("")).is_none());
        assert!(resolve_module_charset_converter(Some("   ")).is_none());
    }

    #[test]
    fn iconv_charset_dot_means_locale_default() {
        let converter = resolve_module_charset_converter(Some(".")).expect("dot should resolve");
        assert!(converter.is_identity());
    }

    #[test]
    fn iconv_charset_comma_with_dot_resolves_to_identity() {
        // upstream: rsync.c:118-120 - server side honours the post-comma value.
        // upstream: rsync.c:125-126 - "." means "use locale default".
        let converter =
            resolve_module_charset_converter(Some("UTF-8,.")).expect("dot remote should resolve");
        assert!(converter.is_identity());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_resolves_simple_charset() {
        let converter =
            resolve_module_charset_converter(Some("ISO-8859-1")).expect("charset should resolve");
        // encoding_rs aliases ISO-8859-1 to windows-1252 internally.
        assert_eq!(converter.local_encoding_name(), "windows-1252");
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_uses_segment_after_comma() {
        // upstream: rsync.c:118-120 - server side honours the post-comma value.
        let converter = resolve_module_charset_converter(Some("UTF-8,ISO-8859-1"))
            .expect("charset should resolve");
        assert_eq!(converter.local_encoding_name(), "windows-1252");
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_returns_none_for_unknown_charset() {
        assert!(resolve_module_charset_converter(Some("not-a-real-charset")).is_none());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_trims_whitespace() {
        let converter = resolve_module_charset_converter(Some("  ISO-8859-1  "))
            .expect("trimmed charset should resolve");
        assert_eq!(converter.local_encoding_name(), "windows-1252");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn iconv_charset_round_trip_latin1_utf8() {
        // Verify the converter actually transcodes correctly: a Latin-1 byte
        // sequence containing U+00E9 ('é' as 0xE9) should round-trip through
        // UTF-8 wire encoding and back.
        let converter =
            resolve_module_charset_converter(Some("ISO-8859-1")).expect("charset should resolve");

        let local_bytes = b"caf\xe9.txt"; // 'café.txt' in Latin-1
        let wire = converter
            .local_to_remote(local_bytes)
            .expect("local_to_remote");
        assert_eq!(wire.as_ref(), "café.txt".as_bytes());

        let round_trip = converter.remote_to_local(&wire).expect("remote_to_local");
        assert_eq!(round_trip.as_ref(), local_bytes);
    }
}

#[cfg(test)]
mod clamped_verbosity_tests {
    use super::{clamp_verbose_flags, clamped_verbose_level};
    use crate::daemon::apply_verbosity;
    use logging::{InfoFlag, info_gte};

    // WHY: upstream gates per-connection log floods with
    // `limit_output_verbosity(lp_max_verbosity(i))` (clientserver.c:1127). Each
    // oc-rsync connection runs on its own worker thread, so the clamped client
    // request must seed that thread's `logging::VerbosityConfig` or every
    // `info_log!`/`debug_log!` emission stays silent. These tests pin the
    // observable gate (`info_gte`) rather than the intermediate count so they
    // fail if the clamp-then-seed chain ever stops controlling log output.
    //
    // Each subtest spawns a fresh thread because `apply_verbosity` writes
    // thread-local state; sharing the harness thread would leak the level into
    // sibling tests.

    fn seed_from_client(flag_string: &str, max_verbosity: i32) {
        let clamped = clamp_verbose_flags(flag_string, max_verbosity);
        apply_verbosity(clamped_verbose_level(&clamped));
    }

    #[test]
    fn level0_request_suppresses_level1_message() {
        // Client asked for no `-v`; a level-1 INFO (e.g. NAME) must not fire.
        std::thread::spawn(|| {
            seed_from_client("-logDtpr", 1);
            assert!(
                !info_gte(InfoFlag::Name, 1),
                "verbosity 0 must suppress the level-1 NAME info message",
            );
        })
        .join()
        .expect("level0 thread");
    }

    #[test]
    fn level1_request_emits_level1_message() {
        // Client asked for `-v` and the module permits it; level-1 INFO fires.
        std::thread::spawn(|| {
            seed_from_client("-logDtprv", 1);
            assert!(
                info_gte(InfoFlag::Name, 1),
                "verbosity 1 must emit the level-1 NAME info message",
            );
        })
        .join()
        .expect("level1 thread");
    }

    #[test]
    fn max_verbosity_clamps_higher_client_request() {
        // Client stacked `-vvv` but the module caps `max verbosity` at 1: the
        // effective level is 1, so a level-2 message stays suppressed even
        // though the client requested far more.
        std::thread::spawn(|| {
            seed_from_client("-logDtprvvv", 1);
            assert!(
                info_gte(InfoFlag::Name, 1),
                "clamped verbosity 1 must still emit the level-1 message",
            );
            assert!(
                !info_gte(InfoFlag::Name, 2),
                "max verbosity 1 must clamp the client's -vvv down to level 1",
            );
        })
        .join()
        .expect("clamp thread");
    }
}
