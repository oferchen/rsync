//! Shell script generation for batch replay.
//!
//! Creates a .sh script that can be used to replay a batch file,
//! matching upstream rsync's format.

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use std::fs::File;
use std::io::{self, Write};

/// Generate a minimal shell script for replaying a batch file.
///
/// Creates a script matching upstream rsync's format: a single command line
/// without a `#!/bin/sh` shebang. The script uses `--read-batch` with a
/// destination placeholder that defaults to the current directory.
///
/// # Upstream Reference
///
/// - `batch.c:255-312`: `write_batch_shell_file()` writes the raw command
///   without a shebang line. The `.sh` file is opened with mode `S_IRUSR |
///   S_IWUSR | S_IXUSR` (0o700).
pub fn generate_script(config: &BatchConfig) -> BatchResult<()> {
    generate_script_with_filters(config, None, None)
}

/// Generate a shell script for replaying a batch file with optional filter rules.
///
/// When `filter_rules` is `Some`, the script includes `--filter=._-` (protocol
/// 29+) or `--exclude-from=-` (older protocols) and appends the rules as a
/// heredoc so that the replay applies the same filters used during batch
/// creation.
///
/// When `destination` is `Some`, it is embedded as the fallback in the
/// `${1:-<dest>}` placeholder so that `./BATCH.sh` (invoked without
/// arguments) writes to the destination that was used when the batch was
/// captured. Mirrors upstream `batch.c:300-304` which writes
/// `${1:-<cooked_argv[argc-1]>}`. When `None`, a bare `.` is used (legacy
/// behavior, only suitable when no destination is known).
///
/// # Upstream Reference
///
/// - `batch.c:205-222`: `write_filter_rules()` embeds filter rules in the
///   shell script using a `<<'#E#'` heredoc.
/// - `batch.c:262-267`: filter option selection based on protocol version.
/// - `batch.c:300-304`: destination placeholder `${1:-<dest>}`.
pub fn generate_script_with_filters(
    config: &BatchConfig,
    filter_rules: Option<&str>,
    destination: Option<&str>,
) -> BatchResult<()> {
    let script_path = config.script_file_path();
    let batch_name = config.batch_file_path().to_string_lossy();
    // upstream: batch.c:254 creates the `.sh` companion through
    // `open_no_attacker_symlinks()` at `S_IRUSR|S_IWUSR|S_IXUSR`. Passing the
    // mode to the create is what makes the script executable; upstream never
    // chmods afterwards, so an existing file keeps whatever mode it had.
    let mut file = crate::operator_file::create_write(
        std::path::Path::new(&script_path),
        crate::operator_file::BATCH_SCRIPT_MODE,
    )
    .map_err(|e| {
        BatchError::Io(io::Error::new(
            e.kind(),
            format!("Failed to create script file '{script_path}': {e}"),
        ))
    })?;

    // upstream: batch.c:259 - write_arg(raw_argv[0]) - the exact binary the
    // user invoked. Without this, the generated BATCH.sh fails with
    // "command not found" when oc-rsync is not on PATH (e.g. test harnesses
    // and CI that invoke via absolute path).
    write!(file, "{}", shell_quote(&config.invoker))?;

    // upstream: batch.c:262-267 - embed filter option before --read-batch
    // so the heredoc at the end of the script feeds rules into stdin
    if filter_rules.is_some() {
        if config.protocol_version >= 29 {
            // upstream: batch.c:263-264 write_opt("--filter", "._-")
            write!(file, " --filter=._-")?;
        } else {
            // upstream: batch.c:265-266 write_opt("--exclude-from", "-")
            write!(file, " --exclude-from=-")?;
        }
    }

    // upstream: batch.c:269-298 - reconstruct the pass-through options from
    // the original invocation (transfer-affecting flags like -a/-z/
    // --numeric-ids) and convert --write-batch to --read-batch. When the raw
    // argv was not captured (legacy callers), fall back to emitting just the
    // --read-batch option so the script still replays the batch file.
    if config.replay_args.is_empty() {
        write!(file, " --read-batch={}", shell_quote(&batch_name))?;
    } else {
        write_replay_options(&mut file, &config.replay_args, &config.operands)?;
    }

    // upstream: batch.c:300-304 - destination placeholder
    // write_opt("${1:-", NULL) + write_arg(dest) + "}". The operand is passed
    // through check_for_hostspec so only the local path (not any host: prefix)
    // lands in the ${1:-<dest>} default.
    write!(file, " ${{1:-")?;
    match destination {
        Some(dest) if !dest.is_empty() => write!(file, "{}", shell_quote(strip_hostspec(dest)))?,
        _ => write!(file, ".")?,
    }
    write!(file, "}}")?;

    // upstream: batch.c:305-306 - append filter rules as heredoc
    if let Some(rules) = filter_rules {
        write_filter_heredoc(&mut file, rules, config.eol_nulls)?;
    }

    writeln!(file)?;

    file.flush()?;

    Ok(())
}

/// Write the trailing filter-rule heredoc into the replay script.
///
/// Mirrors upstream `batch.c:205-222 write_filter_rules()`. Rules arrive
/// newline-terminated; when `eol_nulls` is set (`--from0` / `-0`) each rule is
/// instead terminated by a NUL byte and the whole block is followed by a
/// trailing `;\n`, matching upstream's `eol_nulls` branch byte-for-byte so the
/// replayed `--read-batch` parses the rules the same way the original run did.
///
/// # Upstream Reference
///
/// - `batch.c:209`: `write_sbuf(fd, " <<'#E#'\n")`.
/// - `batch.c:212-217`: per-rule terminator (`0` when `eol_nulls`, else `\n`).
/// - `batch.c:219-220`: trailing `";\n"` after NUL-terminated rules.
/// - `batch.c:221`: closing `#E#` delimiter.
fn write_filter_heredoc(file: &mut File, rules: &str, eol_nulls: bool) -> io::Result<()> {
    // upstream: batch.c:209 write_sbuf(fd, " <<'#E#'\n")
    writeln!(file, " <<'#E#'")?;
    if eol_nulls {
        // upstream: batch.c:212-217 - NUL terminates each rule under eol_nulls.
        for rule in rules.strip_suffix('\n').unwrap_or(rules).split('\n') {
            file.write_all(rule.as_bytes())?;
            file.write_all(&[0])?;
        }
        // upstream: batch.c:219-220 write_sbuf(fd, ";\n")
        file.write_all(b";\n")?;
    } else {
        write!(file, "{rules}")?;
        if !rules.ends_with('\n') {
            writeln!(file)?;
        }
    }
    // upstream: batch.c:221 write_sbuf(fd, "#E#")
    write!(file, "#E#")?;
    Ok(())
}

/// Generate a shell script for replaying a batch file with full argument preservation.
///
/// Converts `--write-batch` / `--only-write-batch` arguments to `--read-batch`,
/// preserves relevant options, and embeds filter rules if present. The output
/// matches upstream rsync's `batch.c:write_batch_shell_file()` format.
///
/// # Upstream Reference
///
/// - `batch.c:255-312`: `write_batch_shell_file()` elides filename args,
///   converts write-batch to read-batch, and embeds filter rules via heredoc.
/// - `batch.c:258-267`: filter rules use `--filter=._-` (protocol >= 29) or
///   `--exclude-from=-` (protocol < 29) to consume the heredoc from stdin.
pub fn generate_script_with_args(
    config: &BatchConfig,
    original_args: &[String],
    filter_rules: Option<&str>,
) -> BatchResult<()> {
    let script_path = config.script_file_path();
    // upstream: batch.c:254 creates the `.sh` companion through
    // `open_no_attacker_symlinks()` at `S_IRUSR|S_IWUSR|S_IXUSR`. Passing the
    // mode to the create is what makes the script executable; upstream never
    // chmods afterwards, so an existing file keeps whatever mode it had.
    let mut file = crate::operator_file::create_write(
        std::path::Path::new(&script_path),
        crate::operator_file::BATCH_SCRIPT_MODE,
    )
    .map_err(|e| {
        BatchError::Io(io::Error::new(
            e.kind(),
            format!("Failed to create script file '{script_path}': {e}"),
        ))
    })?;

    // upstream: batch.c:261 write_arg(raw_argv[0]) - binary name, no shebang
    write!(file, "{}", original_args[0])?;

    // upstream: batch.c:262-267 - if filter rules are present, add the option
    // that tells rsync to read them from stdin (the heredoc appended below)
    if filter_rules.is_some() {
        if config.protocol_version >= 29 {
            // upstream: batch.c:263-264 write_opt("--filter", "._-")
            write!(file, " --filter=._-")?;
        } else {
            // upstream: batch.c:265-266 write_opt("--exclude-from", "-")
            write!(file, " --exclude-from=-")?;
        }
    }

    // upstream: batch.c:270-298 - process arguments, skipping filenames and
    // converting write-batch to read-batch. We iterate with an index to
    // handle bare options that consume the following value argument.
    let args = &original_args[1..];
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if let Some(batch_name) = arg.strip_prefix("--write-batch=") {
            // upstream: batch.c:292-294 convert --write-batch to --read-batch
            write!(file, " --read-batch={}", shell_quote(batch_name))?;
        } else if let Some(batch_name) = arg.strip_prefix("--only-write-batch=") {
            // upstream: batch.c:292-294 convert --only-write-batch to --read-batch
            write!(file, " --read-batch={}", shell_quote(batch_name))?;
        } else if arg == "--write-batch" || arg == "--only-write-batch" {
            // upstream: batch.c:292-294 bare form - next arg is the batch name
            i += 1;
            if i < args.len() {
                write!(file, " --read-batch={}", shell_quote(&args[i]))?;
            }
        } else if arg.starts_with("--files-from")
            || arg.starts_with("--filter")
            || arg.starts_with("--include")
            || arg.starts_with("--exclude")
        {
            // upstream: batch.c:280-283 skip filter/include/exclude options
            if !arg.contains('=') {
                i += 1; // skip the following value argument
            }
        } else if arg == "-f" {
            // upstream: batch.c:288-289 skip -f (filter shortcut) + its value
            i += 1;
        } else {
            // upstream: batch.c:296-297 pass through other arguments
            write!(file, " {}", shell_quote(arg))?;
        }

        i += 1;
    }

    // upstream: batch.c:300-304 write destination placeholder
    // write_opt("${1:-", NULL) + write_arg(dest) + "}"
    write!(file, " ${{1:-")?;
    if let Some(dest) = find_destination(original_args) {
        write!(file, "{}", shell_quote(strip_hostspec(dest)))?;
    }
    write!(file, "}}")?;

    // upstream: batch.c:305-306 write_filter_rules() uses heredoc with #E# delimiter
    if let Some(rules) = filter_rules {
        write_filter_heredoc(&mut file, rules, config.eol_nulls)?;
    }

    writeln!(file)?;

    file.flush()?;

    Ok(())
}

/// Reconstruct the `--read-batch` pass-through options into the replay script.
///
/// This is the single source of truth for which options a batch replay must
/// re-apply. It mirrors the argument-reconstruction loop in upstream rsync's
/// `batch.c:write_batch_shell_file()` (batch.c:269-298):
///
/// - Elides the positional filename operands (`operands`) by reverse-matching
///   them against `replay_args`, exactly as upstream nulls out the trailing
///   `cooked_argv` filenames (batch.c:269-275). The destination is re-supplied
///   by the caller through the `${1:-<dest>}` placeholder.
/// - Strips `--files-from`, `--filter`, `--include`, `--exclude`, and `-f`
///   together with their value arguments; their rules are replayed via the
///   heredoc instead (batch.c:280-291).
/// - Converts `--write-batch`/`--only-write-batch` to `--read-batch`
///   (batch.c:292-294).
/// - Passes through every remaining transfer-affecting flag verbatim (`-a`,
///   `-z`, `--numeric-ids`, ...) so the replay applies identical options
///   (batch.c:295-298).
fn write_replay_options(
    file: &mut File,
    replay_args: &[String],
    operands: &[String],
) -> io::Result<()> {
    // upstream: batch.c:269-275 - null out the trailing filename operands so
    // they are not re-emitted; the destination returns via ${1:-<dest>}.
    let mut elide = vec![false; replay_args.len()];
    let mut oi = operands.len();
    let mut i = replay_args.len();
    while i > 1 && oi > 0 {
        i -= 1;
        if replay_args[i] == operands[oi - 1] {
            elide[i] = true;
            oi -= 1;
        }
    }

    // upstream: batch.c:277-298 - emit the surviving options.
    let mut i = 1;
    while i < replay_args.len() {
        if elide[i] {
            i += 1;
            continue;
        }
        let p = replay_args[i].as_str();
        // upstream: batch.c:280-286 - skip filter/include/exclude/files-from
        // options and their value argument (unless attached with '=').
        if p.starts_with("--files-from")
            || p.starts_with("--filter")
            || p.starts_with("--include")
            || p.starts_with("--exclude")
        {
            if !p.contains('=') {
                i += 1;
            }
            i += 1;
            continue;
        }
        // upstream: batch.c:288-290 - skip -f and its value argument.
        if p == "-f" {
            i += 2;
            continue;
        }
        // upstream: batch.c:292-294 - convert write-batch to read-batch.
        if let Some(name) = p.strip_prefix("--write-batch=") {
            write!(file, " --read-batch={}", shell_quote(name))?;
        } else if let Some(name) = p.strip_prefix("--only-write-batch=") {
            write!(file, " --read-batch={}", shell_quote(name))?;
        } else if p == "--write-batch" || p == "--only-write-batch" {
            // Bare form: the following (non-elided) arg carries the batch name
            // and is passed through on the next iteration, matching upstream's
            // write_opt("--read-batch", NULL).
            write!(file, " --read-batch")?;
        } else {
            // upstream: batch.c:295-298 - pass through all other arguments.
            write!(file, " {}", shell_quote(p))?;
        }
        i += 1;
    }
    Ok(())
}

/// Quote an argument for the generated `.sh` batch wrapper, byte-matching
/// upstream rsync's `write_arg` (batch.c:164-198).
///
/// Quoting is UNCONDITIONAL. Upstream's own comment gives the reason: single
/// quotes keep "every shell metacharacter (backtick, newline, redirection,
/// ...) literal in the replay script". Its 3.4.x predecessor quoted only when
/// `strpbrk` matched a special-character set that omitted the backtick, `<`,
/// `>` and the braces - so a destination path such as ``d`cmd`x`` was emitted
/// bare and the backtick *executed* when the operator ran `BATCH.sh`.
///
/// An embedded `'` closes the quote, is backslash-escaped, then the quote
/// reopens (`'\''`, batch.c:189-192).
///
/// A leading `-opt=` prefix is written bare so the replay script stays
/// readable, but only when every byte from the leading `-` up to the first `=`
/// is `[-_0-9A-Za-z]` (batch.c:169-183). A metacharacter before the `=` marks
/// what upstream calls "an attacker-shaped arg", which is quoted whole.
///
/// upstream: batch.c:164 write_arg()
fn shell_quote(s: &str) -> String {
    let mut result = String::new();
    let mut arg = s;

    // upstream: batch.c:169-183 - the bare "-opt=" prefix is conditional on the
    // option name being a plain token; upstream walks `p` from `arg` to the '='
    // accepting only `-`, `_` and alphanumerics, and emits the prefix only when
    // the walk reaches the '=' (`p == x`).
    if arg.starts_with('-') {
        if let Some(eq) = arg.find('=') {
            if arg[..eq].bytes().all(is_plain_option_byte) {
                result.push_str(&arg[..=eq]);
                arg = &arg[eq + 1..];
            }
        }
    }

    // upstream: batch.c:187-195 - unconditional single-quote wrap.
    result.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            // upstream: batch.c:191 - write "'\\''", i.e. the four bytes '\''
            result.push_str("'\\''");
        } else {
            result.push(ch);
        }
    }
    result.push('\'');
    result
}

/// Bytes upstream accepts in the option name of a bare `-opt=` prefix.
///
/// upstream: batch.c:174-178 - `'-' || '_' || 0-9 || A-Z || a-z`.
fn is_plain_option_byte(b: u8) -> bool {
    b == b'-' || b == b'_' || b.is_ascii_alphanumeric()
}

/// Find the destination path from the argument list (last non-option argument).
fn find_destination(args: &[String]) -> Option<&str> {
    args.iter()
        .rev()
        .find(|arg| !arg.starts_with('-') && !arg.is_empty())
        .map(|s| s.as_str())
}

/// Strip a `host:` / `host::` / `rsync://` prefix from a destination operand,
/// returning only the local path portion written into the generated `.sh`
/// `${1:-<dest>}` default. Local paths (absolute or relative) are returned
/// unchanged.
///
/// Mirrors upstream's `check_for_hostspec` path extraction:
/// - `host:/path` -> `/path`
/// - `host::mod/path` -> `mod/path`
/// - `rsync://host/path` -> `path`
/// - `/local/path`, `rel/path`, `dest` -> unchanged
///
// upstream: options.c:check_for_hostspec / batch.c:300
fn strip_hostspec(dest: &str) -> &str {
    const URL_PREFIX: &str = "rsync://";
    // upstream: options.c:3134 - an rsync:// URL is matched case-insensitively.
    if dest.len() >= URL_PREFIX.len()
        && dest.as_bytes()[..URL_PREFIX.len()].eq_ignore_ascii_case(URL_PREFIX.as_bytes())
    {
        let rest = &dest[URL_PREFIX.len()..];
        if let Some(off) = parse_hostspec_path(rest, true) {
            return &rest[off..];
        }
    }
    // upstream: options.c:3143 - parse_hostspec(s, &path, NULL).
    match parse_hostspec_path(dest, false) {
        Some(off) => {
            // upstream: options.c:3146-3147 - a leading ':' (the daemon '::'
            // separator) is stripped from the returned path.
            let path = &dest[off..];
            path.strip_prefix(':').unwrap_or(path)
        }
        None => dest,
    }
}

/// Locate the path portion of a `[user@]host[:port]` spec, mirroring upstream
/// `parse_hostspec` (options.c:3073). Returns the byte offset in `s` where the
/// path begins when `s` starts with a valid host, or `None` otherwise.
/// `is_url` mirrors upstream's non-NULL `port_ptr` (parsing an `rsync://` host).
fn parse_hostspec_path(s: &str, is_url: bool) -> Option<usize> {
    let b = s.as_bytes();
    let mut host_start = 0usize;
    let mut i = 0usize;
    loop {
        if i >= b.len() {
            // upstream: running out of string is only OK for an rsync:// URL.
            return if is_url { Some(i) } else { None };
        }
        match b[i] {
            b':' | b'/' => {
                let was_slash = b[i] == b'/';
                i += 1;
                if was_slash {
                    if !is_url {
                        return None;
                    }
                } else if is_url {
                    // upstream: atoi(port), then require '/' or end after digits.
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    if i < b.len() {
                        let ch = b[i];
                        i += 1;
                        if ch != b'/' {
                            return None;
                        }
                    }
                }
                return Some(i);
            }
            b'@' => host_start = i + 1,
            b'[' => {
                // upstream: the '[' must be the first host character; the host
                // is an IPv6 literal terminated by ']'.
                if i != host_start {
                    return None;
                }
                host_start += 1;
                i += 1;
                while i < b.len() && b[i] != b']' && b[i] != b'/' {
                    i += 1;
                }
                let hostlen = i - host_start;
                let at_bracket = i < b.len() && b[i] == b']';
                let next_ok = i + 1 >= b.len() || matches!(b[i + 1], b'/' | b':');
                if !at_bracket || !next_ok || hostlen == 0 {
                    return None;
                }
                // Fall through: the outer i += 1 steps past the ']'.
            }
            _ => {}
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BatchMode;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn test_shell_quote() {
        // Quoting is unconditional (batch.c:187-195): even an argument with no
        // metacharacter at all comes back wrapped.
        assert_eq!(shell_quote("simple"), "'simple'");
        assert_eq!(shell_quote("with-dash"), "'with-dash'");
        assert_eq!(shell_quote("/path/to/file"), "'/path/to/file'");
        assert_eq!(shell_quote("needs quoting"), "'needs quoting'");
        // upstream closes, escapes, then reopens: `'\''` (batch.c:189-192).
        assert_eq!(shell_quote("has'quote"), "'has'\\''quote'");
        assert_eq!(shell_quote("$special"), "'$special'");
    }

    /// The metacharacters upstream's 3.4.x `strpbrk` set did NOT cover.
    ///
    /// Each of these was emitted bare by the old conditional quoting, so a
    /// destination path or option value containing one was interpreted by the
    /// shell when the operator ran `BATCH.sh`. The backtick is the live
    /// exploit: the rsync 3.5.0 `write-batch-quoting` cell plants a
    /// destination named ``d`>PWNED`x/`` and fails if the command substitution
    /// runs.
    #[test]
    fn shell_quote_neutralises_the_metacharacters_the_old_set_missed() {
        for arg in [
            "d`>PWNED`x/",
            "a`cmd`b",
            "a>b",
            "a<b",
            "a{b}c",
            "a,b",
            "a+b",
            "user@host",
            "~/dir",
            "50%",
            "a\nb",
        ] {
            assert_eq!(
                shell_quote(arg),
                format!("'{arg}'"),
                "{arg:?} must be quoted whole"
            );
        }
    }

    /// Golden byte parity with upstream `write_arg` (batch.c:164-198).
    #[test]
    fn test_shell_quote_matches_upstream_write_arg() {
        // Every character in the old special set still quotes - the set simply
        // stopped being the deciding factor.
        for c in [
            ' ', '"', '&', ';', '|', '[', ']', '(', ')', '$', '#', '!', '*', '?', '^', '\\',
        ] {
            let arg = format!("a{c}b");
            assert_eq!(shell_quote(&arg), format!("'{arg}'"), "{c:?} must quote");
        }

        // Embedded single quote: close, backslash-escape, reopen
        // (batch.c:189-192).
        assert_eq!(shell_quote("'"), "''\\'''");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("a'b'c"), "'a'\\''b'\\''c'");

        // A leading "-opt=" prefix is written bare so the replay script stays
        // readable; only the value is quote-processed (batch.c:169-183).
        assert_eq!(shell_quote("--filter=- *.tmp"), "--filter='- *.tmp'");
        assert_eq!(shell_quote("--rsh=ssh -p 22"), "--rsh='ssh -p 22'");
        // The value is quoted even when it holds no metacharacter.
        assert_eq!(shell_quote("--opt=a,b+c"), "--opt='a,b+c'");
        // A non-option token with '=' is not split.
        assert_eq!(shell_quote("a=b c"), "'a=b c'");
        assert_eq!(shell_quote("x:y=z"), "'x:y=z'");
        // The bare prefix is conditional: a metacharacter before the '=' marks
        // an attacker-shaped argument, which upstream quotes whole
        // (batch.c:169-183).
        assert_eq!(shell_quote("-a`b=c"), "'-a`b=c'");
        assert_eq!(shell_quote("-a b=c"), "'-a b=c'");
        assert_eq!(shell_quote("-$x=c"), "'-$x=c'");
        // ... while `-`, `_` and alphanumerics keep the readable prefix.
        assert_eq!(shell_quote("--no_iconv=x"), "--no_iconv='x'");
        assert_eq!(shell_quote("-4=x"), "-4='x'");
    }

    #[test]
    fn test_find_destination() {
        let args = vec![
            "rsync".to_owned(),
            "-av".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];
        assert_eq!(find_destination(&args), Some("dest/"));

        let args2 = vec![
            "rsync".to_owned(),
            "--write-batch=batch".to_owned(),
            "-av".to_owned(),
            "src".to_owned(),
        ];
        assert_eq!(find_destination(&args2), Some("src"));
    }

    /// Verify `strip_hostspec` mirrors upstream `check_for_hostspec`, writing
    /// only the local path into the generated `.sh` `${1:-<dest>}` default.
    ///
    /// upstream: batch.c:300 passes the last operand through check_for_hostspec
    /// so a batch captured against a remote destination does not embed the
    /// `host:` prefix in the replay script.
    #[test]
    fn test_strip_hostspec() {
        // Local paths are returned unchanged.
        assert_eq!(strip_hostspec("/path/to/dest"), "/path/to/dest");
        assert_eq!(strip_hostspec("dest"), "dest");
        assert_eq!(strip_hostspec("rel/path"), "rel/path");
        assert_eq!(strip_hostspec("./dir"), "./dir");

        // Remote shell form: host:/path -> /path.
        assert_eq!(strip_hostspec("host:/path"), "/path");
        assert_eq!(strip_hostspec("user@host:/path"), "/path");
        assert_eq!(strip_hostspec("host:rel"), "rel");

        // Daemon form: host::module/path -> module/path.
        assert_eq!(strip_hostspec("host::mod/path"), "mod/path");
        assert_eq!(strip_hostspec("user@host::mod/path"), "mod/path");

        // URL form: rsync://host/path -> path (leading slash is the separator).
        assert_eq!(strip_hostspec("rsync://host/path"), "path");
        assert_eq!(strip_hostspec("rsync://host:873/mod/path"), "mod/path");
        assert_eq!(strip_hostspec("RSYNC://host/path"), "path");

        // IPv6 literal hosts have the bracketed address stripped.
        assert_eq!(strip_hostspec("[::1]:/path"), "/path");
    }

    /// Verify a batch written against a remote destination operand strips the
    /// `host:` prefix from the generated `${1:-<dest>}` default.
    ///
    /// upstream: batch.c:300-304 write_batch_shell_file passes the destination
    /// through check_for_hostspec before writing it.
    #[test]
    fn test_generate_script_strips_remote_destination() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        generate_script_with_filters(&config, None, Some("host:/remote/dest")).unwrap();

        let content = fs::read_to_string(config.script_file_path()).unwrap();
        assert!(
            content.contains("${1:-'/remote/dest'}"),
            "remote host: prefix must be stripped from the default: {content}"
        );
        assert!(
            !content.contains("host:"),
            "generated script must not embed the host: prefix: {content}"
        );
    }

    #[test]
    fn test_generate_script() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let result = generate_script(&config);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        assert!(Path::new(&script_path).exists());

        // upstream batch shell scripts have no shebang.
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            !content.starts_with("#!/bin/sh"),
            "Upstream rsync batch scripts have no shebang"
        );
        assert!(content.contains("--read-batch="));
        assert!(content.contains("oc-rsync"));
    }

    /// Verify the script embeds an absolute invoker path verbatim.
    ///
    /// Upstream `batch.c:259` writes `raw_argv[0]` literally so the replay
    /// script works regardless of `PATH`. The CI testsuite invokes oc-rsync
    /// by absolute path, so the generated BATCH.sh must also use the
    /// absolute path - otherwise the wrapper script fails with
    /// `oc-rsync: command not found`.
    #[test]
    fn test_generate_script_embeds_absolute_invoker_path() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        let absolute_invoker = "/home/runner/work/rsync/rsync/target/release/oc-rsync";

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        )
        .with_invoker(absolute_invoker);

        generate_script(&config).unwrap();

        let content = fs::read_to_string(config.script_file_path()).unwrap();
        let first_line = content.lines().next().expect("script must have content");
        assert!(
            first_line.starts_with(&format!("'{absolute_invoker}'")),
            "wrapper script must start with the configured invoker path \
             (matches upstream batch.c:259 raw_argv[0]); got: {first_line}"
        );
        assert!(first_line.contains("--read-batch="));
    }

    /// Verify shell-unsafe characters in the invoker get quoted.
    #[test]
    fn test_generate_script_quotes_invoker_with_spaces() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        )
        .with_invoker("/path with spaces/oc-rsync");

        generate_script(&config).unwrap();

        let content = fs::read_to_string(config.script_file_path()).unwrap();
        assert!(
            content.starts_with("'/path with spaces/oc-rsync'"),
            "invoker with spaces must be single-quoted: {content}"
        );
    }

    /// Verify the default invoker is the bare `oc-rsync` name for
    /// backwards-compatible callers that don't configure one.
    #[test]
    fn test_generate_script_default_invoker_is_bare_name() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );
        assert_eq!(config.invoker, "oc-rsync");

        generate_script(&config).unwrap();

        let content = fs::read_to_string(config.script_file_path()).unwrap();
        assert!(content.starts_with("'oc-rsync' "));
    }

    #[test]
    fn test_generate_script_with_filters() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch=test.batch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let filter_rules = "- *.tmp\n+ */\n+ *.txt\n- *\n";

        let result = generate_script_with_args(&config, &args, Some(filter_rules));
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        // upstream: batch.c:263-264 adds --filter=._- for protocol >= 29
        assert!(
            content.contains("--filter=._-"),
            "Script must include --filter=._- for protocol >= 29 to consume heredoc: {content}"
        );
        assert!(content.contains("<<'#E#'"));
        assert!(content.contains(filter_rules));
        assert!(content.contains("#E#"));
    }

    /// Verify that filter rules use --exclude-from=- for protocol < 29.
    ///
    /// upstream: batch.c:265-266 write_opt("--exclude-from", "-")
    #[test]
    fn test_generate_script_with_filters_protocol_28() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            28, // protocol < 29
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch=test.batch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let filter_rules = "- *.log\n";

        let result = generate_script_with_args(&config, &args, Some(filter_rules));
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("--exclude-from=-"),
            "Script must include --exclude-from=- for protocol < 29: {content}"
        );
        assert!(!content.contains("--filter=._-"));
    }

    /// Verify that bare --write-batch (without =) is handled correctly.
    ///
    /// upstream: batch.c:292-294 handles both --write-batch=NAME and bare forms.
    #[test]
    fn test_generate_script_bare_write_batch() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch".to_owned(),
            "mybatch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let result = generate_script_with_args(&config, &args, None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("--read-batch='mybatch'"),
            "Bare --write-batch should be converted to --read-batch=<name>: {content}"
        );
        let occurrences = content.matches("mybatch").count();
        assert_eq!(
            occurrences, 1,
            "Batch name should appear exactly once (in --read-batch=): {content}"
        );
    }

    /// Verify that no filter option is added when no filter rules are present.
    #[test]
    fn test_generate_script_no_filters() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch=mybatch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let result = generate_script_with_args(&config, &args, None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            !content.contains("--filter"),
            "No --filter option without filter rules: {content}"
        );
        assert!(
            !content.contains("--exclude-from"),
            "No --exclude-from without filter rules: {content}"
        );
        assert!(
            !content.contains("#E#"),
            "No heredoc without filter rules: {content}"
        );
    }

    /// Verify that --filter and -f args from original command are stripped.
    ///
    /// upstream: batch.c:280-289 skips --filter, --include, --exclude, -f args.
    #[test]
    fn test_generate_script_strips_filter_args() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--filter=._-".to_owned(),
            "--exclude".to_owned(),
            "*.tmp".to_owned(),
            "-f".to_owned(),
            "+ */".to_owned(),
            "--include=*.txt".to_owned(),
            "--write-batch=mybatch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let result = generate_script_with_args(&config, &args, None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            !content.contains("*.tmp"),
            "Excluded patterns should be stripped: {content}"
        );
        assert!(
            !content.contains("+ */"),
            "Filter rule values should be stripped: {content}"
        );
        assert!(
            !content.contains("--include=*.txt"),
            "Include args should be stripped: {content}"
        );
    }

    /// Verify that `generate_script_with_filters` embeds filter rules via heredoc
    /// and adds --filter=._- for protocol >= 29.
    ///
    /// upstream: batch.c:205-222 write_filter_rules() + batch.c:262-267 option.
    #[test]
    fn test_generate_script_with_filters_embeds_heredoc() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let filter_rules = "- *.tmp\n+ */\n+ *.txt\n- *\n";

        let result = generate_script_with_filters(&config, Some(filter_rules), None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(
            content.contains("--filter=._-"),
            "Script must include --filter=._- for protocol >= 29: {content}"
        );
        assert!(content.contains("--read-batch="));
        assert!(content.contains("<<'#E#'"));
        assert!(content.contains(filter_rules));
        assert!(content.contains("#E#"));
        assert!(
            !content.starts_with("#!/bin/sh"),
            "Upstream rsync batch scripts have no shebang"
        );
    }

    /// Verify that `generate_script_with_filters` without rules produces a
    /// clean script with no heredoc or filter options.
    #[test]
    fn test_generate_script_with_filters_none_produces_clean_script() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let result = generate_script_with_filters(&config, None, None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(
            !content.contains("--filter"),
            "No --filter option without filter rules: {content}"
        );
        assert!(
            !content.contains("#E#"),
            "No heredoc without filter rules: {content}"
        );
        assert!(content.contains("--read-batch="));
        assert!(content.contains("oc-rsync"));
    }

    /// Verify that protocol < 29 uses --exclude-from=- instead of --filter=._-.
    ///
    /// upstream: batch.c:265-266 write_opt("--exclude-from", "-")
    #[test]
    fn test_generate_script_with_filters_protocol_28_exclude_from() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            28,
        );

        let filter_rules = "- *.log\n";

        let result = generate_script_with_filters(&config, Some(filter_rules), None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(
            content.contains("--exclude-from=-"),
            "Script must include --exclude-from=- for protocol < 29: {content}"
        );
        assert!(
            !content.contains("--filter=._-"),
            "Should not use --filter for protocol < 29: {content}"
        );
        assert!(content.contains("<<'#E#'"));
        assert!(content.contains("- *.log"));
    }

    /// Verify that `generate_script_with_filters` embeds the supplied
    /// destination as the `${1:-<dest>}` fallback, matching upstream
    /// `batch.c:300-304` (`write_opt("${1:-", NULL) + write_arg(dest) + "}"`).
    ///
    /// Without this, `./BATCH.sh` invoked with no argument falls back to `.`
    /// (the current working directory) instead of the original destination,
    /// breaking the upstream testsuite `batch-mode.test` BATCH.sh test which
    /// expects the captured destination to be written to.
    #[test]
    fn test_generate_script_with_filters_embeds_destination() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let result = generate_script_with_filters(&config, None, Some("/path/to/dest"));
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(
            content.contains("${1:-'/path/to/dest'}"),
            "Script must embed destination in placeholder: {content}"
        );
        assert!(
            !content.contains("${1:-.}"),
            "Script must not fall back to '.' when destination is supplied: {content}"
        );
    }

    /// Verify that destinations containing shell-unsafe characters are
    /// single-quoted in the `${1:-<dest>}` placeholder, matching upstream
    /// `batch.c:303` which calls `write_arg(p)` (single-quotes when needed).
    #[test]
    fn test_generate_script_with_filters_quotes_destination_with_spaces() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let result = generate_script_with_filters(&config, None, Some("/path with spaces/dest"));
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(
            content.contains("${1:-'/path with spaces/dest'}"),
            "Destination with spaces must be single-quoted: {content}"
        );
    }

    /// Verify the legacy `None` destination still writes the `.` fallback so
    /// any caller that has not yet migrated keeps working.
    #[test]
    fn test_generate_script_with_filters_no_destination_falls_back_to_dot() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let result = generate_script_with_filters(&config, None, None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(
            content.contains("${1:-.}"),
            "Without a destination, the script must default to '.': {content}"
        );
    }

    /// Verify the replay script re-emits the transfer-affecting pass-through
    /// options from the original invocation.
    ///
    /// The generated `BATCH.sh` must reconstruct the same options the batch was
    /// recorded with (`-a`, `-z`, `--numeric-ids`, ...); otherwise replaying
    /// the batch applies a different set of transfer semantics than the capture
    /// used, diverging from upstream `batch.c:write_batch_shell_file()`
    /// (batch.c:269-298). Filename operands and filter options must be elided
    /// (the destination returns via `${1:-<dest>}`; filter rules via the
    /// heredoc).
    #[test]
    fn test_generate_script_reconstructs_pass_through_options() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        let batch_name = batch_path.to_string_lossy().into_owned();

        let config = BatchConfig::new(BatchMode::Write, batch_name.clone(), 31)
            .with_invoker("oc-rsync")
            .with_replay_args([
                "oc-rsync".to_owned(),
                "-az".to_owned(),
                "--numeric-ids".to_owned(),
                "--filter=- *.tmp".to_owned(),
                format!("--write-batch={batch_name}"),
                "src/".to_owned(),
                "dst/".to_owned(),
            ])
            .with_operands(["src/".to_owned(), "dst/".to_owned()]);

        generate_script_with_filters(&config, Some("- *.tmp\n"), Some("dst/")).unwrap();

        let content = fs::read_to_string(config.script_file_path()).unwrap();

        // Transfer-affecting flags must be preserved for replay.
        assert!(
            content.contains(" '-az'"),
            "replay script must re-apply -az: {content}"
        );
        assert!(
            content.contains(" '--numeric-ids'"),
            "replay script must re-apply --numeric-ids: {content}"
        );
        // --write-batch is converted to --read-batch.
        assert!(
            content.contains(&format!("--read-batch='{batch_name}'")),
            "replay script must convert --write-batch to --read-batch: {content}"
        );
        assert!(
            !content.contains("--write-batch"),
            "replay script must not retain --write-batch: {content}"
        );
        // Filter options are stripped from the option list (replayed via heredoc).
        assert!(
            !content.contains("--filter=- *.tmp"),
            "raw --filter option must be elided from the command line: {content}"
        );
        assert!(
            content.contains("--filter=._-"),
            "heredoc filter option must be present: {content}"
        );
        // Filename operands are elided; the destination returns via ${1:-<dest>}.
        assert!(
            content.contains("${1:-'dst/'}"),
            "destination must be re-supplied via placeholder: {content}"
        );
        assert!(
            !content.contains(" src/"),
            "source operand must be elided from the command line: {content}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_script_is_executable() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        generate_script(&config).unwrap();

        let script_path = config.script_file_path();
        let metadata = fs::metadata(&script_path).unwrap();
        let permissions = metadata.permissions();
        use std::os::unix::fs::PermissionsExt as _;
        // upstream: batch.c:254 batch_sh_fd opened with
        // S_IRUSR | S_IWUSR | S_IXUSR (0o700)
        assert_eq!(
            permissions.mode() & 0o777,
            0o700,
            "Script permissions should be exactly 0o700"
        );
    }

    /// Verify the filter heredoc NUL-terminates each rule and appends a
    /// trailing `;\n` when `eol_nulls` (`--from0`) is set.
    ///
    /// WHY: upstream `batch.c:212-220` swaps the per-rule `\n` terminator for a
    /// NUL and writes `";\n"` after the last rule under `eol_nulls`. The
    /// replayed `--read-batch` reads its rules with the same NUL separator, so
    /// a newline-terminated heredoc would mis-parse a `--from0` batch.
    #[test]
    fn test_filter_heredoc_honors_eol_nulls() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        )
        .with_eol_nulls(true);

        let filter_rules = "- *.tmp\n+ *.txt\n";
        generate_script_with_filters(&config, Some(filter_rules), None).unwrap();

        let content = fs::read(config.script_file_path()).unwrap();

        // Each rule is NUL-terminated, then a trailing ";\n", then "#E#".
        let expected = b"- *.tmp\0+ *.txt\0;\n#E#";
        let window = content.windows(expected.len()).any(|w| w == expected);
        assert!(
            window,
            "heredoc must NUL-terminate rules and append ;\\n: {:?}",
            String::from_utf8_lossy(&content)
        );
        // The newline-terminated form must not appear when eol_nulls is set.
        assert!(
            !content.windows(b"*.tmp\n".len()).any(|w| w == b"*.tmp\n"),
            "eol_nulls must not leave newline terminators: {:?}",
            String::from_utf8_lossy(&content)
        );
    }

    /// Verify that without `eol_nulls` the heredoc keeps newline terminators
    /// and never emits the `;\n` terminator or NUL bytes.
    ///
    /// WHY: the `;\n` trailer and NUL separators are exclusive to upstream's
    /// `eol_nulls` branch (`batch.c:219`); a default (`--from0`-less) batch
    /// must reproduce the newline-terminated form exactly.
    #[test]
    fn test_filter_heredoc_default_uses_newlines() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let filter_rules = "- *.tmp\n+ *.txt\n";
        generate_script_with_filters(&config, Some(filter_rules), None).unwrap();

        let content = fs::read(config.script_file_path()).unwrap();
        assert!(!content.contains(&0u8), "no NUL bytes without eol_nulls");

        let text = String::from_utf8(content).unwrap();
        assert!(text.contains("- *.tmp\n+ *.txt\n#E#"));
        assert!(
            !text.contains(";\n#E#"),
            "no trailing ;\\n without eol_nulls: {text}"
        );
    }
}
