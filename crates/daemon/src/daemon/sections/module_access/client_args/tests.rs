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
    use super::collapse_under_root;
    use std::path::{Path, PathBuf};
    use test_support::COLLAPSE_CASES;

    /// Every upstream `clean_fname(name, CFN_COLLAPSE_DOT_DOT_DIRS)` case must
    /// collapse identically here.
    ///
    /// `collapse_under_root` folds a client-supplied `--link-dest` /
    /// `--copy-dest` / `--compare-dest` basis into a module-relative tail. A
    /// `..` that fails to consume the component before it would leave a path
    /// that still *looks* module-relative while resolving elsewhere - exactly
    /// the shape of upstream's off-by-one, which left the collapse dead for
    /// every multi-component and absolute path.
    ///
    /// The table is shared (`test_support::COLLAPSE_CASES`) so a new edge case
    /// is one row and reaches every oc-rsync copy of this rule at once.
    // upstream: util1.c clean_fname() CFN_COLLAPSE_DOT_DOT_DIRS; t_clean_fname.c
    #[test]
    fn upstream_collapse_cases_consume_the_preceding_component() {
        for (input, expected) in COLLAPSE_CASES {
            // The shared table spells its expectations for a resolver that
            // keeps a root or a bare `.`; this one emits a module-relative
            // tail, so strip the leading `/` and read an empty tail as `.`.
            let want = expected.strip_prefix('/').unwrap_or(expected);
            let want = if want == "." { "" } else { want };
            assert_eq!(
                collapse_under_root(Path::new(input)),
                PathBuf::from(want),
                "clean_fname collapse case {input:?}"
            );
        }
    }

    /// A `..` with nothing left to pop is DISCARDED, not preserved.
    ///
    /// That is upstream's rule, not a shortcut: a daemon runs
    /// `sanitize_path()` at depth 0 (`options.c:2405`, gated on
    /// `sanitize_paths` which `clientserver.c:1068` sets for every daemon
    /// connection), so `util1.c:1183`'s `if (depth <= 0 || sanp != start)` arm
    /// always wins and there is no arm that refuses. Discarding is what makes
    /// the output closed under the module root by construction, which is what
    /// lets `clamp_basis_to_module` join it onto the root with no separate
    /// containment check - and it is why a client's `--link-dest=../sibling`
    /// becomes an in-module `sibling` that then draws upstream's
    /// `arg does not exist` warning instead of being dropped in silence.
    // upstream: util1.c:1183-1191 sanitize_path(), depth 0
    #[test]
    fn unpoppable_leading_dot_dot_is_discarded_at_the_root() {
        assert_eq!(
            collapse_under_root(Path::new("../outside")),
            PathBuf::from("outside")
        );
        assert_eq!(
            collapse_under_root(Path::new("mod/../../outside")),
            PathBuf::from("outside")
        );
        // Every level of an all-`..` climb is consumed, leaving the root
        // itself rather than any ancestor of it.
        assert_eq!(
            collapse_under_root(Path::new("../../..")),
            PathBuf::from("")
        );
    }
}

#[cfg(test)]
mod daemon_partial_dir_arg_tests {
    use super::{ServerConfig, ServerRole, apply_long_form_args};
    use std::ffi::OsString;
    use std::path::Path;

    fn parse(client_args: &[&str]) -> ServerConfig {
        let mut config = ServerConfig::from_flag_string_and_args(
            ServerRole::Receiver,
            "-logDtpre.iLsfxCIvu".to_owned(),
            vec![OsString::from(".")],
        )
        .expect("server flag string parses");
        let owned: Vec<String> = client_args.iter().map(|a| (*a).to_owned()).collect();
        let unknown = apply_long_form_args(&owned, &mut config);
        assert_eq!(unknown, None, "no arg in {client_args:?} is client-only");
        config
    }

    /// The spelling upstream actually puts on the wire.
    ///
    /// upstream: `options.c:3052-3056` - `server_options()` emits
    /// `--partial-dir` and its value as TWO argv entries via
    /// `safe_arg("", partial_dir)`, exactly as it does for `--temp-dir` and
    /// `--backup-dir`. Without this arm the value fell through to the
    /// positional region and the daemon receiver staged nowhere.
    #[test]
    fn split_partial_dir_reaches_the_daemon_receiver() {
        let config = parse(&["--partial-dir", "pdir", "--delay-updates"]);
        assert_eq!(
            config.partial_dir.as_deref(),
            Some(Path::new("pdir")),
            "the split spelling upstream emits must reach the receiver",
        );
        assert!(config.has_partial_dir);
        assert!(config.write.delay_updates);
    }

    /// oc's own daemon client emits the joined spelling
    /// (`daemon_transfer/orchestration/arguments.rs`), and upstream's popt
    /// accepts it too, so both must resolve to the same configuration.
    #[test]
    fn joined_partial_dir_reaches_the_daemon_receiver() {
        let config = parse(&["--partial-dir=pdir", "--delay-updates"]);
        assert_eq!(config.partial_dir.as_deref(), Some(Path::new("pdir")));
        assert!(config.has_partial_dir);
    }

    /// Non-vacuity companion: the two assertions above are attributable to the
    /// option and not to a default. Without `--partial-dir` the receiver has no
    /// staging directory even when `--delay-updates` is present - which is
    /// precisely the state the missing arm used to produce for both spellings.
    #[test]
    fn delay_updates_alone_leaves_no_partial_dir() {
        let config = parse(&["--delay-updates"]);
        assert_eq!(config.partial_dir, None);
        assert!(!config.has_partial_dir);
        assert!(config.write.delay_updates);
    }
}

#[cfg(test)]
mod daemon_partial_dir_sanitize_tests {
    use super::{clamp_basis_to_module, collapse_relative_within_depth, sanitize_partial_dir};
    use std::path::{Path, PathBuf};

    /// upstream: `util1.c:1184-1197` `sanitize_path()` - the `..` arms.
    ///
    /// `depth` budgets LEADING `..` only. Upstream keeps a virtual start that
    /// advances past each allowed `../`, so consecutive leading `..` each
    /// consume one unit while budget lasts, and a `..` that pops the output
    /// back to empty re-enters the leading state.
    #[test]
    fn relative_collapse_matches_upstream_depth_budget() {
        // depth 0 - upstream's `if (depth <= 0 || sanp != start)` always wins,
        // so every `..` either pops or is discarded, and nothing escapes.
        for (given, want) in [
            ("pdir", "pdir"),
            ("a/b", "a/b"),
            ("a/../b", "b"),
            ("../pdir", "pdir"),
            ("../../pdir", "pdir"),
            ("a/../../pdir", "pdir"),
        ] {
            assert_eq!(
                collapse_relative_within_depth(Path::new(given), 0),
                PathBuf::from(want),
                "depth 0: {given:?}"
            );
        }

        // depth 1 - exactly one LEADING `..` survives.
        assert_eq!(
            collapse_relative_within_depth(Path::new("../pdir"), 1),
            PathBuf::from("../pdir")
        );
        // The second one exhausts the budget and is discarded.
        assert_eq!(
            collapse_relative_within_depth(Path::new("../../pdir"), 1),
            PathBuf::from("../pdir")
        );
        // A `..` AFTER a real component pops instead of being kept, even with
        // budget remaining - upstream's `sanp != start` half of the guard.
        assert_eq!(
            collapse_relative_within_depth(Path::new("a/../pdir"), 1),
            PathBuf::from("pdir")
        );
        // ...but once that pop empties the output, the leading state returns
        // and the budget applies again.
        assert_eq!(
            collapse_relative_within_depth(Path::new("a/../../pdir"), 1),
            PathBuf::from("../pdir")
        );

        // depth 2 - consecutive leading `..` each consume one unit.
        assert_eq!(
            collapse_relative_within_depth(Path::new("../../pdir"), 2),
            PathBuf::from("../../pdir")
        );

        // upstream: util1.c:1203-1206 - an empty result becomes ".".
        assert_eq!(
            collapse_relative_within_depth(Path::new("a/.."), 0),
            PathBuf::from(".")
        );
    }

    /// THE DEFECT THIS FIXES: a relative `--partial-dir` must stay RELATIVE.
    ///
    /// `partial_dir_fname()` re-anchors the value at `dirname(fname)` for every
    /// entry, so an absolute result pins every entry's staging directory at the
    /// transfer root. Upstream leaves a relative value relative
    /// (`util1.c:1145-1151` applies the rootdir only inside `if (*p == '/')`),
    /// and a nested entry then stages at `<dest>/sub/pdir`.
    ///
    /// Measured against real rsync 3.5.0 over a daemon push with
    /// `--delay-updates --partial-dir=pdir` into `mod/sub/`: upstream consumes
    /// `<module>/sub/pdir`, oc staged at `<module>/pdir` and left `sub/pdir`
    /// untouched.
    #[test]
    fn a_relative_partial_dir_stays_relative() {
        let module_root = Path::new("/srv/mod");
        let dest = Path::new("/srv/mod/sub");

        assert_eq!(
            sanitize_partial_dir(Path::new("pdir"), dest, module_root),
            PathBuf::from("pdir"),
            "a relative --partial-dir must not be anchored at the destination"
        );
        // The destination's depth below the module root IS upstream's
        // `curr_dir_depth`, so one `..` is affordable from `mod/sub`.
        assert_eq!(
            sanitize_partial_dir(Path::new("../pdir"), dest, module_root),
            PathBuf::from("../pdir")
        );
        // At the module root the budget is 0, so the same value collapses
        // rather than climbing out.
        assert_eq!(
            sanitize_partial_dir(Path::new("../pdir"), module_root, module_root),
            PathBuf::from("pdir")
        );
    }

    /// Non-vacuity companion: the ABSOLUTE arm must still re-root at the module,
    /// exactly as `--backup-dir` and the alt-basis dirs do.
    ///
    /// Without this the pin above would also hold for a fix that simply stopped
    /// clamping altogether, which would let `--partial-dir=/pdir` address the
    /// filesystem root - the defect PR #7498 closed.
    #[test]
    fn an_absolute_partial_dir_still_re_roots_at_the_module() {
        let module_root = Path::new("/srv/mod");
        let dest = Path::new("/srv/mod/sub");

        assert_eq!(
            sanitize_partial_dir(Path::new("/pdir"), dest, module_root),
            PathBuf::from("/srv/mod/pdir")
        );
        assert_eq!(
            sanitize_partial_dir(Path::new("/etc/pdir"), dest, module_root),
            PathBuf::from("/srv/mod/etc/pdir")
        );
        // And it agrees with the basis clamp on that arm, which is why the
        // absolute case delegates rather than reimplementing.
        assert_eq!(
            sanitize_partial_dir(Path::new("/pdir"), dest, module_root),
            clamp_basis_to_module(Path::new("/pdir"), dest, module_root)
        );
    }

    /// `has_root()`, NOT `is_absolute()`: upstream's test is the literal byte
    /// `*p == '/'` (`util1.c:1145`) applied to a PEER-SUPPLIED path.
    ///
    /// On Windows `Path::is_absolute()` is FALSE for `/pdir` because there is
    /// no drive prefix, so an `is_absolute()` gate routes a peer-sent absolute
    /// value down the RELATIVE arm and never re-roots it at the module root.
    /// The sibling test above cannot see that: on Unix the two spellings
    /// agree, so it passes either way. This one pins the predicate so the
    /// divergence cannot be reintroduced by "simplifying" back.
    #[test]
    fn a_leading_slash_partial_dir_re_roots_on_every_platform() {
        let module_root = Path::new("/srv/mod");
        let dest = Path::new("/srv/mod/sub");

        for given in ["/pdir", "/etc/pdir"] {
            assert!(
                Path::new(given).has_root(),
                "{given:?} must be recognised as rooted on this platform"
            );
            let got = sanitize_partial_dir(Path::new(given), dest, module_root);
            assert!(
                got.starts_with(module_root),
                "{given:?} must re-root under {module_root:?}, got {got:?}"
            );
        }
    }

    /// The two consumers genuinely need different answers for a RELATIVE value:
    /// the basis clamp anchors once and returns an absolute path, the partial
    /// dir must not. Pinning the disagreement is what stops the two being
    /// "simplified" back into one call.
    #[test]
    fn the_basis_clamp_and_the_partial_dir_disagree_on_a_relative_value() {
        let module_root = Path::new("/srv/mod");
        let dest = Path::new("/srv/mod/sub");

        assert_eq!(
            clamp_basis_to_module(Path::new("pdir"), dest, module_root),
            PathBuf::from("/srv/mod/sub/pdir"),
            "the basis clamp folds the destination in, as its consumer expects"
        );
        assert_eq!(
            sanitize_partial_dir(Path::new("pdir"), dest, module_root),
            PathBuf::from("pdir"),
            "the partial dir must not, because its consumer re-anchors per file"
        );
    }
}
