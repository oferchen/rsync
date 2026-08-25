/// Gate: every long option the daemon client EMITS must be BRIDGED by the
/// daemon's server-argument parser.
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
/// Known gaps live in `parse_bridge_registry.txt`, keyed on the OPTION NAME
/// with a reason. The baseline is therefore ZERO for anything new: an option
/// that is not in the registry is red the moment it stops being bridged. The
/// registry is deliberately NOT a count - a numeric baseline freezes in the
/// very rows the gate exists to surface.
#[cfg(test)]
mod parse_bridge_gate {
    use super::apply_long_form_args;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use super::ServerConfig;

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

    /// Every `--long-option` spelling the production emitter can put on the wire.
    fn emitted_option_names() -> Vec<String> {
        let src = std::fs::read_to_string(emitter_source_path())
            .expect("daemon client argument emitter must be readable");
        let production = strip_test_modules(&src);
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
        let text =
            std::fs::read_to_string(registry_path()).expect("parse-bridge registry must be readable");
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

    /// Feeds one option to the real parser and reports whether it moved anything.
    ///
    /// Both the joined (`--opt=value`) and split (`--opt value`) spellings are
    /// tried, plus the bare boolean form, because the daemon parser handles the
    /// families inconsistently and a bridge that only accepts one spelling still
    /// counts as bridged for the option as a whole.
    fn is_bridged(name: &str) -> bool {
        let baseline = ServerConfig::default();
        // The parser skips everything at or after the standalone `.`
        // separator, so a bare option with no `.` is not the shape it sees on
        // the wire. Mirror a real client argv.
        let candidates: [Vec<String>; 3] = [
            vec![format!("--{name}"), ".".to_owned(), "mod/p".to_owned()],
            vec![format!("--{name}=1"), ".".to_owned(), "mod/p".to_owned()],
            vec![
                format!("--{name}"),
                "1".to_owned(),
                ".".to_owned(),
                "mod/p".to_owned(),
            ],
        ];
        candidates.iter().any(|args| {
            let mut config = ServerConfig::default();
            let _ = apply_long_form_args(args, &mut config);
            config != baseline
        })
    }

    /// Every option the daemon client emits must reach `ServerConfig`, or be a
    /// REGISTERED gap with a stated reason.
    ///
    /// ⚠ The registry is keyed on option NAME and carries no count. Adding a gap
    /// requires naming it; it cannot be absorbed into a numeric baseline.
    #[test]
    fn every_emitted_daemon_option_is_bridged_or_registered() {
        let registered = registry();
        let mut unregistered_gaps: Vec<String> = Vec::new();
        let mut stale_registry_entries: Vec<String> = Vec::new();

        for name in emitted_option_names() {
            let bridged = is_bridged(&name);
            match (bridged, registered.get(&name)) {
                (false, None) => unregistered_gaps.push(name),
                (true, Some(_)) => stale_registry_entries.push(name),
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
            "these options ARE bridged now but are still listed as gaps - remove \
             them from parse_bridge_registry.txt so the registry keeps shrinking: \
             {stale_registry_entries:?}"
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

}
