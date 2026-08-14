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
mod output_verbosity_limit_tests {
    use super::{ServerConfig, ServerRole, apply_output_verbosity_limit};
    use crate::daemon::apply_verbosity;
    use logging::{InfoFlag, info_gte};
    use std::ffi::OsString;

    // WHY: upstream gates per-connection log floods with
    // `limit_output_verbosity(lp_max_verbosity(i))` (clientserver.c:1141), which
    // lowers the `info_levels[]`/`debug_levels[]` OUTPUT arrays
    // (options.c:537-560) and leaves the client's option string untouched. Two
    // separate invariants ride on that shape and both are pinned here:
    //
    //   1. The `v` inside the `-e.<caps>` payload is NOT a `-v`. It is the
    //      client's `CF_VARINT_FLIST_FLAGS` / negotiated-strings advertisement
    //      (compat.c:730-733), and upstream's popt gives everything after `-e`
    //      to `shell_cmd` (options.c:823) rather than treating it as option
    //      letters. Counting it inflates every connection's verbosity by one.
    //   2. Limiting must not rewrite the option string. A rewrite that dropped
    //      that `v` would silently strip the peer's capability advertisement -
    //      a wire divergence, not a logging one.
    //
    // Each subtest that seeds the thread-local `logging::VerbosityConfig` spawns
    // a fresh thread; sharing the harness thread would leak the level into
    // sibling tests.

    // The exact compact bundle an oc/upstream client sends at DEFAULT verbosity
    // for `-a` over a daemon transport, capability suffix included.
    const DEFAULT_CLIENT_BUNDLE: &str = "-logDtpre.iLsfxCIvu";

    fn parse(flag_string: &str) -> ServerConfig {
        ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            flag_string.to_owned(),
            vec![OsString::from(".")],
        )
        .expect("server flag string parses")
    }

    #[test]
    fn capability_suffix_v_is_not_a_verbose_request() {
        let mut cfg = parse(DEFAULT_CLIENT_BUNDLE);
        let level = apply_output_verbosity_limit(&mut cfg, 1);
        assert_eq!(
            level, 0,
            "the `v` in the -e.<caps> payload is the client's CF_VARINT_FLIST_FLAGS \
             advertisement, not a -v: a default-verbosity connection must run at level 0",
        );
        assert!(!cfg.flags.verbose);
    }

    #[test]
    fn limiting_never_rewrites_the_capability_suffix() {
        // `max verbosity = 0` is the strongest limit an operator can set; it
        // must still leave the `-e` payload byte-identical, because that payload
        // is what the capability decoder turns into the compat flags written on
        // the wire (compat.c:712-738).
        // `1` is the shipped default (upstream daemon-parm.txt:49), `0` the
        // strongest limit an operator can set, `-1` the below-zero arm.
        for max_verbosity in [-1, 0, 1, 2, 5, 9] {
            for bundle in [
                DEFAULT_CLIENT_BUNDLE,
                "-vlogDtpre.iLsfxCIvu",
                "-vvvlogDtpre.iLsfxCIvu",
            ] {
                let mut cfg = parse(bundle);
                apply_output_verbosity_limit(&mut cfg, max_verbosity);
                assert_eq!(
                    cfg.flag_string, bundle,
                    "max verbosity {max_verbosity} must cap the level, not edit the \
                     client's option string",
                );
                // Spelled out rather than left implicit in the equality above:
                // every capability letter the peer advertised has to survive,
                // because each one is a compat-flag bit (compat.c:720-733).
                let payload = cfg
                    .flag_string
                    .split_once('e')
                    .expect("bundle carries an -e argument")
                    .1;
                for letter in ['i', 'L', 's', 'f', 'x', 'C', 'I', 'v', 'u'] {
                    assert!(
                        payload.contains(letter),
                        "capability letter `{letter}` was stripped at \
                         max verbosity {max_verbosity}",
                    );
                }
            }
        }
    }

    #[test]
    fn max_verbosity_caps_a_higher_client_request() {
        // Client stacked `-vvv` but the module caps `max verbosity` at 1.
        let mut cfg = parse("-vvvlogDtpre.iLsfxCIvu");
        assert_eq!(cfg.flags.verbose_level, 3, "client asked for -vvv");
        let level = apply_output_verbosity_limit(&mut cfg, 1);
        assert_eq!(level, 1, "max verbosity 1 caps the client's -vvv");
        assert_eq!(cfg.flag_string, "-vvvlogDtpre.iLsfxCIvu");

        std::thread::spawn(move || {
            apply_verbosity(level);
            assert!(
                info_gte(InfoFlag::Name, 1),
                "capped verbosity 1 must still emit the level-1 NAME message",
            );
            assert!(
                !info_gte(InfoFlag::Name, 2),
                "max verbosity 1 must suppress the level-2 NAME message",
            );
        })
        .join()
        .expect("cap thread");
    }

    #[test]
    fn zero_max_verbosity_silences_a_verbose_client() {
        let mut cfg = parse("-vlogDtpre.iLsfxCIvu");
        let level = apply_output_verbosity_limit(&mut cfg, 0);
        assert_eq!(level, 0);
        assert!(!cfg.flags.verbose);

        std::thread::spawn(move || {
            apply_verbosity(level);
            assert!(
                !info_gte(InfoFlag::Name, 1),
                "max verbosity 0 must suppress the level-1 NAME info message",
            );
        })
        .join()
        .expect("zero thread");
    }

    #[test]
    fn permissive_max_verbosity_passes_the_request_through() {
        // upstream: options.c:542-543 - `limit_output_verbosity` returns without
        // applying any limit once the module's cap exceeds MAX_VERBOSITY, so the
        // client's own level survives intact.
        let mut cfg = parse("-vlogDtpre.iLsfxCIvu");
        assert_eq!(apply_output_verbosity_limit(&mut cfg, 9), 1);

        let mut cfg = parse("-vvlogDtpre.iLsfxCIvu");
        assert_eq!(apply_output_verbosity_limit(&mut cfg, 5), 2);
    }

    #[test]
    fn negative_max_verbosity_silences_the_connection() {
        // A negative `max verbosity` makes upstream's `for (j = 0; j <= level;)`
        // loop body never run (options.c:548), leaving every limit at zero.
        let mut cfg = parse("-vvlogDtpre.iLsfxCIvu");
        assert_eq!(apply_output_verbosity_limit(&mut cfg, -1), 0);
    }
}

#[cfg(test)]
mod daemon_module_suffix_tests {
    use super::{ModuleDefinition, ServerConfig, apply_module_transfer_directives};

    /// Selecting a module is what makes upstream's `module_id >= 0`, and that
    /// is the only condition under which `full_fname()` appends
    /// ` (in MODULE)` to a quoted path. Recording the name here is what lets a
    /// daemon-side `send_files failed to open` / `mkstemp failed` line name the
    /// module the client asked for, exactly as upstream 3.4.4 does.
    ///
    /// upstream: clientserver.c:821 `module_id = i`; util1.c:1290
    /// `if (module_id >= 0)`.
    #[test]
    fn module_selection_records_name_for_full_fname() {
        let module = ModuleDefinition {
            name: "mymod".to_string(),
            ..ModuleDefinition::default()
        };
        let mut cfg = ServerConfig::default();
        assert_eq!(cfg.connection.daemon_module, None);

        apply_module_transfer_directives(&module, &mut cfg);

        assert_eq!(cfg.connection.daemon_module.as_deref(), Some("mymod"));
    }

    /// A `ServerConfig` that never passed through module selection stays at
    /// upstream's `module_id < 0`, so no suffix is rendered. This is the SSH
    /// `--server` and client-side shape.
    #[test]
    fn config_without_module_selection_has_no_module_name() {
        assert_eq!(ServerConfig::default().connection.daemon_module, None);
    }
}

#[cfg(test)]
mod daemon_clean_fname_collapse_tests {
    use super::lexically_normalize;
    use std::path::{Path, PathBuf};
    use test_support::COLLAPSE_CASES;

    /// Every upstream `clean_fname(name, CFN_COLLAPSE_DOT_DOT_DIRS)` case must
    /// collapse identically here.
    ///
    /// `lexically_normalize` folds a client-supplied `--link-dest` /
    /// `--copy-dest` / `--compare-dest` basis before
    /// `confine_basis_under_module` tests it against the module root. The
    /// containment check is a `starts_with` on the folded path, so a `..` that
    /// fails to consume the component before it leaves a path that still
    /// *looks* like it sits under the module while resolving elsewhere. That is
    /// exactly the shape of upstream's off-by-one, which left the collapse dead
    /// for every multi-component and absolute path.
    ///
    /// The table is shared (`test_support::COLLAPSE_CASES`) so a new edge case
    /// is one row and reaches every oc-rsync copy of this rule at once.
    // upstream: util1.c clean_fname() CFN_COLLAPSE_DOT_DOT_DIRS; t_clean_fname.c
    #[test]
    fn upstream_collapse_cases_consume_the_preceding_component() {
        for (input, expected) in COLLAPSE_CASES {
            assert_eq!(
                lexically_normalize(Path::new(input)),
                PathBuf::from(expected),
                "clean_fname collapse case {input:?}"
            );
        }
    }

    /// A `..` with nothing left to pop survives into the result so the
    /// caller's `starts_with(module_root)` check sees the escape and rejects
    /// the basis. Silently swallowing it would hand the caller a path that
    /// passes containment while naming a directory outside the module.
    // upstream: util1.c clean_fname() - "collapse '..' elements (except at the start)"
    #[test]
    fn unpoppable_leading_dot_dot_is_preserved_for_the_containment_check() {
        assert_eq!(
            lexically_normalize(Path::new("../outside")),
            PathBuf::from("../outside")
        );
        assert_eq!(
            lexically_normalize(Path::new("mod/../../outside")),
            PathBuf::from("../outside")
        );
    }
}
