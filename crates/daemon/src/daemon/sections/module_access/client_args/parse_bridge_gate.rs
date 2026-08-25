/// Gate: every long option the daemon client EMITS must be handled by the
/// daemon's server-argument path.
///
/// The two server-argument parsers fail in different ways, so one check cannot
/// cover both (task 982):
///
/// - the SSH decoder parses into `ServerLongFlags` and can silently never
/// bridge, so its defect is a field with no reader;
/// - this parser writes straight into `&mut ServerConfig`, so parse == bridge
/// by construction and its defect is having NO ARM AT ALL - the option falls
/// through the `_` catch-all and is silently ignored.
///
/// This gate covers the second shape, and it is BEHAVIOURAL rather than a
/// grep: it feeds each emitted option to the real parser and asserts the
/// `ServerConfig` actually changed. An arm that exists but writes nothing fails
/// it exactly like a missing arm, which a source scan could not distinguish.
///
/// ⚠ The probe calls `build_server_config`, NOT `apply_long_form_args`. The
/// daemon bridges client options at TWO sites: the long-form parser, and
/// `build_server_config` itself, which reads `--bwlimit` straight out of
/// `client_args` and caps it against the daemon-wide limiter. Probing only the
/// inner parser reported `bwlimit` as a gap when it is measurably live (task
/// 986 paced it in all five cells), so the narrower probe would have written a
/// FALSE row into the registry - a registry that lies with a reason attached is
/// worse than no registry.
///
/// Known gaps live in `parse_bridge_registry.txt`, keyed on the OPTION NAME
/// with a reason. The baseline is therefore ZERO for anything new: an option
/// that is not in the registry is red the moment it stops being handled. The
/// registry is deliberately NOT a count - a numeric baseline freezes in the
/// very rows the gate exists to surface.
#[cfg(test)]
mod parse_bridge_gate {
    use super::ModuleDefinition;
    use super::ModuleRequestContext;
    use super::ServerConfig;
    use super::build_server_config;
    use super::{AdvertisedDigests, ConnectionState, DaemonStream, LegacyMessageCache, ModuleRuntime};
    use std::collections::BTreeMap;
    use std::io::BufReader;
    use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};

    /// Path to the daemon client's argument emitter - the honest population of
    /// options this parser can ever be asked to handle.
    fn emitter_source_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../core/src/client/remote/daemon_transfer/orchestration/arguments.rs")
    }

    fn registry_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/daemon/sections/module_access/client_args/parse_bridge_registry.txt")
    }

    /// Strips `#[cfg(test)]` modules so a test fixture's option spellings are not
    /// mistaken for production emissions.
    ///
    /// The emitter file interleaves several test modules with production code, so
    /// truncating at the first `#[cfg(test)]` would UNDER-count the real set.
    fn strip_test_modules(src: &str) -> String {
        let mut kept = String::new();
        let mut lines = src.lines().peekable();
        while let Some(line) = lines.next() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                // Skip forward to the module's opening brace, then to its match.
                let mut depth = 0usize;
                let mut opened = false;
                for body in lines.by_ref() {
                    depth += body.matches('{').count();
                    if depth > 0 {
                        opened = true;
                    }
                    depth = depth.saturating_sub(body.matches('}').count());
                    if opened && depth == 0 {
                        break;
                    }
                }
                continue;
            }
            kept.push_str(line);
            kept.push('\n');
        }
        kept
    }

    /// Drops whole-line comments so prose spellings are not read as emissions.
    ///
    /// The emitter documents upstream's `opt = "--foo"` convention in a rustdoc
    /// block and repeats it in an inline `// upstream:` note. Those are the only
    /// source of the option name `foo`, which no daemon parser will ever see on
    /// the wire, so leaving them in put a phantom row in the gap set.
    fn strip_comment_lines(src: &str) -> String {
        src.lines()
            .filter(|line| {
                let t = line.trim_start();
                !(t.starts_with("//") || t.starts_with('*') || t.starts_with("/*"))
            })
            .map(|line| format!("{line}\n"))
            .collect()
    }

    /// Every `--long-option` spelling the production emitter can put on the wire.
    fn emitted_option_names() -> Vec<String> {
        let src = std::fs::read_to_string(emitter_source_path())
            .expect("daemon client argument emitter must be readable");
        let production = strip_comment_lines(&strip_test_modules(&src));
        let mut names: Vec<String> = Vec::new();
        for (idx, _) in production.match_indices("\"--") {
            let rest = &production[idx + 1..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap_or(rest.len());
            // `rest` still carries the option's own `--`; the registry and the
            // parser both key on the BARE name, so strip it here. Capturing the
            // dashes silently produced `----opt` candidates that could never
            // match any arm, which reads as "every option is a gap".
            let name = rest[..end].trim_start_matches('-');
            // `--server` / `--sender` are mode markers the daemon strips before
            // option handling, not bridgeable options.
            let is_mode_marker = name == "server" || name == "sender";
            if name.len() > 1 && !is_mode_marker && !names.iter().any(|n| n == name) {
                names.push(name.to_owned());
            }
        }
        names.sort();
        names
    }

    /// Reads the registry as `name -> reason`.
    fn registry() -> BTreeMap<String, String> {
        let text = std::fs::read_to_string(registry_path())
            .expect("parse-bridge registry must be readable");
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let (name, reason) = line
                    .split_once(char::is_whitespace)
                    .unwrap_or_else(|| panic!("registry line needs `name<space>reason`: {line}"));
                (name.to_owned(), reason.trim().to_owned())
            })
            .collect()
    }

    /// What the daemon's server-argument path did with one option.
    ///
    /// `Refused` is deliberately distinct from `Dropped`: an option the daemon
    /// rejects with a diagnostic is HANDLED, just not accepted. Only `Dropped` -
    /// accepted and silently ignored - is the defect this gate exists to catch.
    #[derive(Debug, PartialEq, Eq)]
    enum Handling {
        Bridged,
        Refused,
        Dropped,
    }

    /// Runs `f` with a `ModuleRequestContext` backed by a live loopback socket.
    ///
    /// `build_server_config` touches the reader only on its rejection paths (to
    /// frame an `@ERROR` to the peer), so a connected pair is enough; the client
    /// end is held for the whole call so those writes do not fail spuriously.
    fn with_context<R>(f: impl FnOnce(&mut ModuleRequestContext<'_>) -> R) -> R {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let _client = TcpStream::connect(addr).expect("connect loopback");
        let (server, _) = listener.accept().expect("accept loopback");
        let mut reader = BufReader::new(DaemonStream::plain(server));
        let mut limiter = None;
        let mut session_exit_code = None;
        let mut ctx = ModuleRequestContext {
            reader: &mut reader,
            limiter: &mut limiter,
            peer_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            session_peer_host: None,
            module_peer_host: None,
            reverse_lookup: false,
            request: "mod",
            log_sink: None,
            messages: LegacyMessageCache::shared(),
            early_input_data: None,
            client_digests: AdvertisedDigests::Absent,
            session_exit_code: &mut session_exit_code,
            conn_state: ConnectionState::Transferring,
        };
        f(&mut ctx)
    }

    fn fixture_module(root: &Path) -> ModuleRuntime {
        let definition = ModuleDefinition {
            name: "mod".to_owned(),
            path: root.to_path_buf(),
            ..Default::default()
        };
        ModuleRuntime::new(definition, None)
    }

    /// Candidate values tried for every option that takes one.
    ///
    /// ⚠ A SINGLE value cannot probe this parser. Several arms write to
    /// `ServerConfig` only when the value itself parses - `--usermap=`/
    /// `--groupmap=` need a real id map, `--log-format=` only reacts to `%i`/
    /// `%I`, and `--max-alloc=` rejects anything under 1 MiB. Probing with just
    /// `1` reported all four as silently dropped when their arms are present and
    /// correct. Each value below exists to satisfy one such family; an option
    /// counts as bridged if ANY of them moves the config.
    const PROBE_VALUES: &[&str] = &[
        "1",           // plain counters and booleans
        "%i%I",        // --log-format / --out-format itemize specs
        "0:0",         // --usermap / --groupmap id maps
        "md5",         // --checksum-choice and other named algorithms
        "1048576",     // --max-alloc, which rejects anything below 1 MiB
        "utf-8,utf-8", // --iconv charset pairs
        "*.gz",        // --skip-compress suffix lists
    ];

    /// Feeds one option to the daemon's server-argument path and reports what
    /// happened to it.
    ///
    /// The comparison is DIFFERENTIAL - the same argv with and without the
    /// option - rather than against `ServerConfig::default()`, because the outer
    /// site also applies module directives and path resolution that have nothing
    /// to do with the option under test.
    ///
    /// Both roles and three spellings are tried: some options bridge only for a
    /// sender or only for a receiver, and the parser handles the joined
    /// (`--opt=value`) and split (`--opt value`) families inconsistently. Any one
    /// of them landing counts as handled for the option as a whole.
    fn classify(name: &str) -> Handling {
        let root = tempfile::TempDir::new().expect("fixture module root");
        let module = fixture_module(root.path());
        // One socket for the whole sweep: `build_server_config` touches the
        // reader only to frame a rejection, so the pair is reusable.
        with_context(|ctx| {
            let mut refused = false;
            for role in [&["--server", "--sender"][..], &["--server"][..]] {
                let mut base: Vec<String> = role.iter().map(|s| (*s).to_owned()).collect();
                base.push("-r".to_owned());
                let tail = [".".to_owned(), "mod/p".to_owned()];

                let mut baseline_args = base.clone();
                baseline_args.extend_from_slice(&tail);
                let baseline = build_server_config(ctx, &baseline_args, &module, None)
                    .ok()
                    .flatten();

                let mut spellings: Vec<Vec<String>> = vec![vec![format!("--{name}")]];
                for value in PROBE_VALUES {
                    spellings.push(vec![format!("--{name}={value}")]);
                    spellings.push(vec![format!("--{name}"), (*value).to_owned()]);
                }
                for spelling in spellings {
                    let mut args = base.clone();
                    args.extend(spelling);
                    args.extend_from_slice(&tail);
                    let candidate = build_server_config(ctx, &args, &module, None).ok().flatten();
                    match candidate {
                        // A rejection returns `None` where the baseline built a
                        // config: the option was recognised and refused.
                        None if baseline.is_some() => refused = true,
                        candidate if candidate != baseline => return Handling::Bridged,
                        _ => {}
                    }
                }
            }
            if refused {
                Handling::Refused
            } else {
                Handling::Dropped
            }
        })
    }

    /// Every option the daemon client emits must reach `ServerConfig`, or be a
    /// REGISTERED gap with a stated reason.
    ///
    /// ⚠ The registry is keyed on option NAME and carries no count. Adding a gap
    /// requires naming it; it cannot be absorbed into a numeric baseline.
    #[test]
    fn every_emitted_daemon_option_is_handled_or_registered() {
        let registered = registry();
        let mut unregistered_gaps: Vec<String> = Vec::new();
        let mut stale_registry_entries: Vec<String> = Vec::new();

        for name in emitted_option_names() {
            let dropped = classify(&name) == Handling::Dropped;
            match (dropped, registered.get(&name)) {
                (true, None) => unregistered_gaps.push(name),
                (false, Some(_)) => stale_registry_entries.push(name),
                _ => {}
            }
        }

        assert!(
            unregistered_gaps.is_empty(),
            "these forwarded options never reach ServerConfig and are NOT in the \
             registry - add an arm to the daemon parser, or register the gap with a \
             reason in parse_bridge_registry.txt: {unregistered_gaps:?}"
        );
        assert!(
            stale_registry_entries.is_empty(),
            "these options ARE handled now but are still listed as gaps - remove \
             them from parse_bridge_registry.txt so the registry keeps shrinking: \
             {stale_registry_entries:?}"
        );
    }

    /// NON-VACUITY COMPANION for the whole probe: `--bwlimit` must be seen as
    /// bridged, and must never re-enter the registry.
    ///
    /// It is the one option proven live by measurement rather than by reading -
    /// task 986 paced it correctly in all five transfer cells - AND it is bridged
    /// exclusively at the outer site, inside `build_server_config` itself. So it
    /// is the single row that discriminates between probing the whole
    /// server-argument path and probing only `apply_long_form_args`: under the
    /// narrower probe this test fails. If the probe is ever narrowed again, or
    /// the outer bridge is deleted, this goes red instead of quietly growing the
    /// registry by one false row.
    #[test]
    fn bwlimit_is_bridged_at_the_outer_site_and_is_not_a_registered_gap() {
        assert_eq!(
            classify("bwlimit"),
            Handling::Bridged,
            "--bwlimit is bridged inside build_server_config (it is capped against \
             the daemon-wide limiter there, not in apply_long_form_args), and it is \
             measurably live - so seeing it as a gap means the probe is aimed at the \
             wrong function"
        );
        assert!(
            !registry().contains_key("bwlimit"),
            "--bwlimit is bridged, so it must never appear in the gap registry - a \
             registered row here would document a defect that does not exist"
        );
    }

    /// The gate is worthless if its inputs are empty, so pin both populations.
    ///
    /// A registry that fails to load, or an emitter whose literals stop being
    /// extractable, would otherwise make the check above pass vacuously.
    #[test]
    fn parse_bridge_gate_inputs_are_non_empty() {
        let emitted = emitted_option_names();
        assert!(
            emitted.len() > 40,
            "emitter extraction collapsed - got {} option names, so the gate would \
             pass without checking anything",
            emitted.len()
        );
        assert!(
            emitted.iter().any(|n| n == "delete"),
            "a known-emitted option is missing from the extraction, so the scan is \
             not seeing the production emitter"
        );
        assert!(
            !emitted.iter().any(|n| n.starts_with('-')),
            "extracted names still carry their leading dashes, so every candidate \
             would be `----opt` and NOTHING could ever match an arm: {emitted:?}"
        );
        assert!(
            !emitted.iter().any(|n| n == "server" || n == "sender"),
            "mode markers leaked into the option set - they are stripped before \
             option handling and can never be bridged"
        );
        assert!(
            !emitted.iter().any(|n| n == "foo"),
            "a comment-only spelling leaked into the option set - `--foo` appears \
             only in the emitter's prose about upstream's `opt = \"--foo\"` \
             convention, so it would sit in the gap set forever as a phantom row"
        );
    }

    /// `strip_test_modules` must remove test regions WITHOUT truncating the
    /// production code that follows them.
    ///
    /// The emitter file interleaves four test modules with live code; an extractor
    /// that stopped at the first `#[cfg(test)]` would silently miss every option
    /// emitted below it, and the gate would go quiet on exactly those rows.
    #[test]
    fn strip_test_modules_keeps_production_code_after_a_test_module() {
        let src = concat!(
            "let a = \"--alpha\";\n",
            "#[cfg(test)]\n",
            "mod t {\n    fn f() { let x = \"--from-a-test\"; }\n}\n",
            "let b = \"--beta\";\n",
        );
        let kept = strip_test_modules(src);
        assert!(kept.contains("--alpha"), "production code before the test module was dropped");
        assert!(kept.contains("--beta"), "production code AFTER the test module was dropped");
        assert!(
            !kept.contains("--from-a-test"),
            "test-module content leaked into the production scan"
        );
    }

    /// `strip_comment_lines` must drop prose spellings and keep code ones.
    #[test]
    fn strip_comment_lines_drops_prose_spellings_only() {
        let src = concat!(
            "/// upstream writes `opt = \"--from-rustdoc\"` here\n",
            "    // upstream: safe_arg(\"--from-inline\", value)\n",
            "     * \"--from-block-continuation\"\n",
            "    args.push(\"--from-code\".to_owned());\n",
        );
        let kept = strip_comment_lines(src);
        assert!(kept.contains("--from-code"), "a real emission was stripped as a comment");
        for prose in ["--from-rustdoc", "--from-inline", "--from-block-continuation"] {
            assert!(!kept.contains(prose), "comment spelling {prose} survived the strip");
        }
    }
}
