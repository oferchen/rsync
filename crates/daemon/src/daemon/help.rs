use core::branding::{Brand, manifest, source_line};

/// Column at which every option description starts.
///
/// upstream generates `help-rsyncd.h` from `rsync.1.md` with each description
/// aligned to column 25, so reproducing the alignment keeps the two help texts
/// visually identical for the options they share.
const DESCRIPTION_COLUMN: usize = 25;

/// Renders the daemon help text for the supplied branding profile.
///
/// The shape mirrors upstream `usage.c:daemon_usage()`: the version banner, a
/// blank line, the `Usage:` line, the option list, a blank line, then the
/// two-line trailer steering operators who did not mean to start a daemon.
///
/// Options upstream lists that this daemon's parser rejects - `--dparam=OVERRIDE,
/// -M`, `--log-file-format=FMT` and `--sockopts=OPTIONS` - are omitted rather
/// than advertised: a help text naming an option the parser refuses is worse
/// than a shorter one. See [`push_upstream_options`].
///
/// upstream: usage.c:daemon_usage
pub(crate) fn help_text(brand: Brand) -> String {
    let program = brand.daemon_program_name();
    let default_config = brand.daemon_config_path_str();
    let config_name = config_file_name(default_config);

    let mut text = format!(
        "{program} {version}\n{source_line}\n\nUsage: {program} --daemon [OPTION]...\n",
        version = manifest().rust_version(),
        source_line = source_line(),
    );

    push_upstream_options(&mut text, config_name);
    push_extension_options(&mut text);
    push_trailer(&mut text, program, config_name, default_config);

    text
}

/// Returns the final path component of `path`, the name operators know the
/// daemon configuration file by (`rsyncd.conf`, `oc-rsyncd.conf`, ...).
///
/// The branded configuration paths are `/`-separated constants on every
/// platform, so splitting on `/` is exact rather than platform-dependent.
fn config_file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Appends one `option<padding>description` row.
///
/// Options longer than [`DESCRIPTION_COLUMN`] would leave no separator, so the
/// padding is floored at one space. No current row is anywhere near that wide.
fn push_option(text: &mut String, option: &str, description: &str) {
    let padding = DESCRIPTION_COLUMN.saturating_sub(option.len()).max(1);
    text.push_str(option);
    for _ in 0..padding {
        text.push(' ');
    }
    text.push_str(description);
    text.push('\n');
}

/// Appends the options shared with upstream, in `help-rsyncd.h` order and
/// wording.
///
/// Upstream's `--dparam=OVERRIDE, -M`, `--log-file-format=FMT` and
/// `--sockopts=OPTIONS` rows are absent because `RuntimeOptions::parse_with_brand`
/// rejects those spellings; they are tracked as feature gaps, not hidden here.
///
/// upstream: help-rsyncd.h
fn push_upstream_options(text: &mut String, config_name: &str) {
    push_option(text, "--daemon", "run as an rsync daemon");
    push_option(text, "--address=ADDRESS", "bind to the specified address");
    push_option(text, "--bwlimit=RATE", "limit socket I/O bandwidth");
    push_option(
        text,
        "--config=FILE",
        &format!("specify alternate {config_name} file"),
    );
    push_option(text, "--no-detach", "do not detach from the parent");
    push_option(text, "--port=PORT", "listen on alternate port number");
    push_option(text, "--log-file=FILE", "override the \"log file\" setting");
    push_option(text, "--verbose, -v", "increase verbosity");
    push_option(text, "--ipv4, -4", "prefer IPv4");
    push_option(text, "--ipv6, -6", "prefer IPv6");
    push_option(
        text,
        "--help, -h",
        "show this help (when used with --daemon)",
    );
    push_option(text, "--version, -V", "print the version and exit");
}

/// Appends the daemon options this build accepts that upstream does not.
///
/// Kept in its own labelled section so an operator reading the list can tell at
/// a glance which spellings will not work against upstream `rsync --daemon`.
/// This mirrors the client `--help` layout, which carries the same header.
fn push_extension_options(text: &mut String) {
    text.push('\n');
    text.push_str("oc-rsync extensions (not present in upstream rsync):\n");
    push_option(text, "--bind=ADDRESS", "alias for --address");
    push_option(
        text,
        "--detach",
        "detach from the parent (the default on Unix)",
    );
    push_option(text, "--no-verbose", "reset verbosity to the default");
    push_option(
        text,
        "--no-bwlimit",
        "remove any bandwidth limit set so far",
    );
    push_option(text, "--once", "accept a single connection and exit");
    push_option(
        text,
        "--max-sessions=N",
        "accept N connections and exit (N > 0)",
    );
    push_option(
        text,
        "--max-connections=N",
        "override the \"max connections\" setting",
    );
    push_option(
        text,
        "--module=SPEC",
        "register an in-memory module (NAME=PATH[,COMMENT])",
    );
    push_option(
        text,
        "--motd-file=FILE",
        "override the \"motd file\" setting",
    );
    push_option(
        text,
        "--motd-line=TEXT",
        "append TEXT as an extra motd line",
    );
    push_option(
        text,
        "--lock-file=FILE",
        "override the \"lock file\" setting",
    );
    push_option(
        text,
        "--secrets-file=FILE",
        "override the \"secrets file\" setting",
    );
    push_option(text, "--pid-file=FILE", "override the \"pid file\" setting");
    push_option(
        text,
        "--tcp-fastopen=MODE",
        "enable TCP Fast Open (auto, on, off; default auto)",
    );

    if cfg!(windows) {
        push_option(
            text,
            "--windows-service",
            "run under the Windows Service Control Manager",
        );
        push_option(
            text,
            "--install-service",
            "register the Windows Service and exit",
        );
        push_option(
            text,
            "--uninstall-service",
            "remove the Windows Service registration and exit",
        );
    }
}

/// Appends upstream's closing advice.
///
/// upstream ends with "See also the rsyncd.conf(5) manpage."; this build names
/// the configuration file and its default location instead, because that is
/// what an operator can actually go and read here.
///
/// upstream: usage.c:daemon_usage
fn push_trailer(text: &mut String, program: &str, config_name: &str, default_config: &str) {
    text.push('\n');
    text.push_str(&format!(
        "If you were not trying to invoke {program} as a daemon, avoid using any of the\n\
         daemon-specific {program} options.  See also the {config_name} configuration\n\
         file (default {default_config}).\n",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line the upstream testsuite looks for: `misc-coverage_test.py`
    /// asserts `--daemon --help` names the daemon invocation, which is how it
    /// distinguishes daemon usage from client usage.
    ///
    /// upstream: usage.c:daemon_usage - `Usage: rsync --daemon [OPTION]...`
    #[test]
    fn usage_line_names_the_daemon_invocation() {
        for (brand, program) in [(Brand::Upstream, "rsync"), (Brand::Oc, "oc-rsync")] {
            let text = help_text(brand);
            assert!(
                text.contains(&format!("Usage: {program} --daemon [OPTION]...\n")),
                "{brand:?} usage line must name --daemon, got:\n{text}"
            );
        }
    }

    /// Under the upstream brand every shared row must read exactly as
    /// `help-rsyncd.h` prints it, alignment included. Comparing whole lines -
    /// rather than asserting a substring is present - is what makes a reworded
    /// or misaligned row fail.
    #[test]
    fn shared_option_rows_match_upstream_verbatim() {
        let text = help_text(Brand::Upstream);
        for row in [
            "--daemon                 run as an rsync daemon",
            "--address=ADDRESS        bind to the specified address",
            "--bwlimit=RATE           limit socket I/O bandwidth",
            "--config=FILE            specify alternate rsyncd.conf file",
            "--no-detach              do not detach from the parent",
            "--port=PORT              listen on alternate port number",
            "--log-file=FILE          override the \"log file\" setting",
            "--verbose, -v            increase verbosity",
            "--ipv4, -4               prefer IPv4",
            "--ipv6, -6               prefer IPv6",
            "--help, -h               show this help (when used with --daemon)",
        ] {
            assert!(
                text.lines().any(|line| line == row),
                "missing verbatim upstream row {row:?} in:\n{text}"
            );
        }
    }

    /// The oc brand substitutes its own configuration file name into the row
    /// upstream spells `rsyncd.conf`, so the text never points an operator at a
    /// file this build does not read.
    #[test]
    fn config_row_names_the_branded_config_file() {
        assert!(
            help_text(Brand::Oc).lines().any(
                |line| line == "--config=FILE            specify alternate oc-rsyncd.conf file"
            ),
            "oc help must name oc-rsyncd.conf"
        );
    }

    /// Extensions are listed under their own header so they are never mistaken
    /// for options upstream `rsync --daemon` would accept.
    #[test]
    fn extensions_are_listed_below_their_own_header() {
        let text = help_text(Brand::Oc);
        let header = text
            .find("oc-rsync extensions (not present in upstream rsync):")
            .expect("extensions header");
        let upstream_row = text.find("--no-detach ").expect("upstream row");
        let extension_row = text.find("--max-sessions=N").expect("extension row");
        assert!(
            upstream_row < header && header < extension_row,
            "extensions must follow the header, which must follow the shared rows:\n{text}"
        );
    }

    /// upstream closes by telling an operator who did not mean `--daemon` what
    /// to do next; dropping that leaves the shorter list looking like the whole
    /// of the daemon's configuration surface.
    #[test]
    fn trailer_redirects_a_non_daemon_invocation() {
        let text = help_text(Brand::Oc);
        assert!(
            text.contains(
                "If you were not trying to invoke oc-rsync as a daemon, avoid using any of the\n\
                 daemon-specific oc-rsync options."
            ),
            "missing the upstream trailer in:\n{text}"
        );
        assert!(
            text.contains("/etc/oc-rsync/oc-rsyncd.conf"),
            "trailer must name the default configuration path in:\n{text}"
        );
    }

    /// The defect this file previously carried was a help text describing a
    /// daemon that no longer existed. Every option row must therefore name a
    /// spelling the parser accepts - the assertion the old containment tests
    /// could not make.
    /// `--daemon` is the mode selector, consumed by the front end before the
    /// daemon option parser runs; that this help text was reached at all is
    /// what proves it works.
    const PARSED_BY_THE_FRONT_END: &str = "--daemon";

    #[test]
    fn every_listed_option_is_accepted_by_the_parser() {
        for option in listed_options(&help_text(Brand::Oc)) {
            if option == PARSED_BY_THE_FRONT_END {
                continue;
            }
            assert!(
                option_is_recognised(&option),
                "help lists {option} but the daemon argument pipeline rejects it"
            );
        }
    }

    /// Asks the real two-layer pipeline whether `option` is recognised.
    ///
    /// `parse_args` claims `--help`, `--version` and the service flags and
    /// passes everything else through as `remainder`, which is exactly what the
    /// daemon does in production - so an option is recognised if either layer
    /// took it.
    fn option_is_recognised(option: &str) -> bool {
        let argv = [
            std::ffi::OsString::from("oc-rsync"),
            std::ffi::OsString::from(option),
        ];
        let Ok(parsed) = super::super::parse_args(argv) else {
            return false;
        };
        !parsed.remainder.iter().any(|arg| arg == option)
            || super::super::RuntimeOptions::option_is_recognised(option)
    }

    /// Extracts every option spelling an option row advertises - the long form
    /// and any short alias - dropping the `=VALUE` suffix and the separating
    /// comma. A row's option tokens are its leading dash-prefixed words, which
    /// holds because no description here starts with a dash.
    fn listed_options(text: &str) -> Vec<String> {
        text.lines()
            .filter(|line| line.starts_with("--"))
            .flat_map(|line| {
                line.split_whitespace()
                    .take_while(|token| token.starts_with('-'))
                    .map(|token| {
                        let token = token.trim_end_matches(',');
                        token.split('=').next().unwrap_or(token).to_owned()
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Guards the extractor itself: a `listed_options` that silently dropped
    /// rows - or dropped every short alias, which is where `-h` was missing -
    /// would make the parser check above pass vacuously.
    #[test]
    fn the_option_extractor_finds_long_and_short_spellings() {
        let options = listed_options(&help_text(Brand::Oc));
        for expected in ["--daemon", "--max-sessions", "-h", "-4", "-v"] {
            assert!(
                options.iter().any(|option| option == expected),
                "extractor missed {expected} in {options:?}"
            );
        }
        let rows = help_text(Brand::Oc)
            .lines()
            .filter(|line| line.starts_with("--"))
            .count();
        assert!(
            options.len() > rows,
            "extractor found no short aliases at all: {options:?}"
        );
    }
}
