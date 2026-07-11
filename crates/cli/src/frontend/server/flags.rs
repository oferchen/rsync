//! Server-mode long flag parsing.
//!
//! Extracts `--flag` and `--flag=value` arguments from the server argument
//! list into a structured representation.

use std::ffi::OsString;
use std::path::PathBuf;

use engine::{ReferenceDirectory, ReferenceDirectoryKind};

/// Detects whether secluded-args mode is requested in the server arguments.
///
/// Upstream rsync's `server_options()` (options.c:2604) embeds `s` in the
/// compact flag string when `protect_args` is active - e.g.
/// `-slogDtprze.iLsfxCIvu`. The `s` appears in the transfer-flag portion
/// (before the first `.`), never in the capability/info suffix.
///
/// This function checks both standalone `-s` and `s` embedded in compact
/// flag arguments (single-dash args that are not long flags).
///
/// upstream: options.c:792 - `{"secluded-args", 's', ...}`
pub(crate) fn detect_secluded_args_flag(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|a| {
        let s = a.to_string_lossy();
        if s == "-s" {
            return true;
        }
        // Check compact flag strings: starts with `-`, not `--`.
        // Only scan the transfer-flag portion (before the first `.`)
        // because `s` after the dot is the symlink-iconv capability char,
        // not secluded-args.
        // upstream: options.c:2604 - protect_args placed at argstr[1]
        if s.starts_with('-') && !s.starts_with("--") && s.len() > 1 {
            let transfer_portion = s[1..].split('.').next().unwrap_or("");
            return transfer_portion.contains('s');
        }
        false
    })
}

/// Long-form flags extracted from the server argument list.
///
/// These correspond to the `--flag` and `--flag=value` arguments that
/// upstream rsync's `server_options()` emits alongside the compact flag string.
/// upstream: options.c - `server_options()`.
pub(super) struct ServerLongFlags {
    pub(super) is_sender: bool,
    pub(super) is_receiver: bool,
    pub(super) ignore_errors: bool,
    pub(super) fsync: bool,
    pub(super) io_uring_policy: fast_io::IoUringPolicy,
    /// Optional `--io-uring-depth=N` value forwarded by the client.
    pub(super) io_uring_depth: Option<String>,
    pub(super) zero_copy_policy: fast_io::ZeroCopyPolicy,
    pub(super) write_devices: bool,
    pub(super) trust_sender: bool,
    /// Keep partially transferred files (upstream: `--partial`, long-form only).
    ///
    /// upstream: options.c:2893 - `server_options()` emits bare `--partial`
    /// (never a compact `P` letter) when the client is the sender and no
    /// `--partial-dir` is configured. The receiver consults `keep_partial`
    /// (receiver.c) to leave interrupted temp files in place.
    pub(super) partial: bool,
    /// Explicit specials override (upstream: `--specials` / `--no-specials`).
    ///
    /// upstream: options.c:2760-2765 - the compact `D` letter carries
    /// preserve_devices only, so specials arrive separately: `--specials`
    /// (`Some(true)`) or `--no-specials` (`Some(false)`). `None` leaves the
    /// value implied by the compact `D` letter untouched.
    pub(super) specials: Option<bool>,
    pub(super) qsort: bool,
    pub(super) checksum_seed: Option<String>,
    pub(super) checksum_choice: Option<String>,
    pub(super) min_size: Option<String>,
    pub(super) max_size: Option<String>,
    /// Memory allocation cap forwarded by the client.
    ///
    /// upstream: options.c:2845-2846 - `--max-alloc=arg` is emitted by
    /// `server_options()` when the user-supplied value differs from the
    /// default. Each side enforces its own cap, so the server records and
    /// applies the value locally.
    pub(super) max_alloc: Option<String>,
    pub(super) stop_at: Option<String>,
    pub(super) stop_after: Option<String>,
    pub(super) files_from: Option<String>,
    pub(super) from0: bool,
    pub(super) inplace: bool,
    pub(super) size_only: bool,
    /// Modification-time window in whole seconds (upstream: `--modify-window=NUM`).
    ///
    /// upstream: options.c - `server_options()` emits `--modify-window=%d`
    /// whenever the client set a non-default `modify_window`. The receiver's
    /// quick-check consults this via `same_time()` so files within the window
    /// are not needlessly re-transferred.
    pub(super) modify_window: Option<String>,
    /// Numeric IDs only (upstream: `--numeric-ids`, long-form only).
    pub(super) numeric_ids: bool,
    /// Delete extraneous files (upstream: `--delete-*` variants, long-form only).
    pub(super) delete: bool,
    /// Remove source files after a successful transfer.
    ///
    /// upstream: options.c:2964-2965 - `server_options()` emits
    /// `--remove-source-files` (or the legacy alias `--remove-sent-files`)
    /// whenever the client asked for sender-side removal. The flag is
    /// long-form only; the sender's `successful_send()` reads the global
    /// `remove_source_files` to decide whether to unlink each file after
    /// the receiver acknowledges a successful transfer.
    pub(super) remove_source_files: bool,
    /// Whether `--stats` was forwarded by the client.
    ///
    /// upstream: options.c:2838-2839 - `server_options()` emits `--stats` whenever
    /// the client requested detailed statistics. The server-side `do_stats` flag
    /// gates emission of `NDX_DEL_STATS` during the goodbye phase, which the
    /// client relies on for the "Number of deleted files" stats line.
    pub(super) stats: bool,
    /// Skip updating files that exist at destination (upstream: `--ignore-existing`).
    pub(super) ignore_existing: bool,
    /// Skip creating files not present at destination (upstream: `--existing`).
    pub(super) existing_only: bool,
    /// Maximum deletions allowed (upstream: `--max-delete=NUM`).
    pub(super) max_delete: Option<String>,
    /// Iconv specification forwarded by the client (upstream: `--iconv=CHARSET`).
    ///
    /// upstream: options.c:2716-2723 - client forwards the post-comma half of
    /// `--iconv=LOCAL,REMOTE` (or the whole spec if no comma) so the server
    /// opens its own iconv context against the wire's UTF-8 charset.
    pub(super) iconv: Option<String>,
    /// I/O timeout in seconds forwarded by the client (upstream: `--timeout=N`).
    ///
    /// upstream: options.c - `server_options()` emits `--timeout=%d` whenever
    /// the client has `io_timeout` set. Recognising it here keeps it out of
    /// the positional-argument list; without this the value lands in
    /// `parse_server_flag_string_and_args` and corrupts the destination path.
    pub(super) timeout: Option<String>,
    /// Reference directories for basis file lookup.
    /// upstream: options.c:2915-2923 - `--compare-dest`, `--copy-dest`, `--link-dest`
    pub(super) reference_directories: Vec<ReferenceDirectory>,
    /// Raw `--info=FLAGS` values forwarded by the client.
    ///
    /// upstream: options.c:2928-2931 - `server_options()` emits the output of
    /// `make_output_option(info_words, info_levels, ...)`, so the server
    /// receives `--info=...` whenever the client has non-default info levels.
    pub(super) info: Vec<OsString>,
    /// Explicit compression algorithm forwarded by the client.
    ///
    /// upstream: options.c:2800-2805 - `server_options()` emits long-form
    /// `--new-compress` (zlibx), `--old-compress` (zlib), or
    /// `--compress-choice=ALGO` (zstd/lz4) whenever the negotiated codec is
    /// not the default CPRES_ZLIB carried by the compact `-z` flag. Capturing
    /// the value here lets the server bypass vstring negotiation and use the
    /// same algorithm as the client.
    pub(super) compress_choice: Option<String>,
    /// Compression level forwarded by the client (`--compress-level=N`).
    ///
    /// upstream: options.c:2754-2758 - `server_options()` emits
    /// `--compress-level=%d` whenever `do_compression` is active and the level
    /// differs from the unspecified default. The raw `N` string is captured so
    /// the server can apply the same level to its codec; without recognising
    /// the flag the scanner treated `--compress-level=6` as a positional
    /// destination path (`failed to create destination root --compress-level=6`).
    pub(super) compression_level: Option<String>,
    /// Log format forwarded by the client (upstream: `--log-format=FMT`).
    ///
    /// upstream: options.c:2750-2762 - the client sends `--log-format=%i`
    /// (or `%i%I`, `%o`, `X`) so the server knows whether the generator
    /// should produce itemize data. The server does not use the full format
    /// string - it only inspects it for `%i` / `%o` tokens to set
    /// `stdout_format_has_i` and `stdout_format_has_o_or_i`.
    pub(super) log_format: Option<String>,
    /// Partial-directory path forwarded by the client (`--partial-dir=DIR`).
    ///
    /// upstream: options.c:2886-2890 - `server_options()` emits the option
    /// as TWO separate argv entries (`--partial-dir`, then the value) via
    /// `safe_arg("", partial_dir)`, NOT the `--partial-dir=VALUE` form.
    /// Without recognising the split form, the value lands in
    /// `parse_server_flag_string_and_args` and gets parsed as a positional
    /// destination path - the receiver then creates a directory literally
    /// named `--partial-dir` at the transfer root and never honours the
    /// partial-dir semantics. Issue #715 regression test
    /// (`symlink-dirlink-basis_test.py` test 7) drives this path with
    /// `--protocol=28 --partial-dir=.rsync-partial`.
    pub(super) partial_dir: Option<OsString>,
    /// Whether `--delay-updates` was forwarded by the client.
    ///
    /// upstream: options.c:2891-2892 - `server_options()` emits
    /// `--delay-updates` alongside `--partial-dir` when both are active.
    pub(super) delay_updates: bool,
    /// Whether `--mkpath` was forwarded by the client (upstream: `--mkpath`).
    ///
    /// upstream: options.c:2996-2997 - `if (mkpath_dest_arg && am_sender)
    /// args[ac++] = "--mkpath"`. Long-form only. Gates the receiver's
    /// dest-arg path creation (`main.c:736` `make_path` vs `main.c:788`
    /// single `do_mkdir`).
    pub(super) mkpath: bool,
    /// Whether the client forwarded `--list-only` (upstream `list_only > 1`).
    ///
    /// upstream: options.c:2747-2748 - forwarded so the peer knows the transfer
    /// is a listing. The receiver renders the flist without writing to the
    /// destination.
    pub(super) list_only: bool,
}

/// Parses all long-form flags from the server argument list.
///
/// Scans the argument list for `--flag` and `--flag=value` arguments,
/// extracting their values into a structured result. Unknown long flags
/// are ignored for forward compatibility.
pub(super) fn parse_server_long_flags(args: &[OsString]) -> ServerLongFlags {
    let mut flags = ServerLongFlags {
        is_sender: false,
        is_receiver: false,
        ignore_errors: false,
        fsync: false,
        io_uring_policy: fast_io::IoUringPolicy::Auto,
        io_uring_depth: None,
        zero_copy_policy: fast_io::ZeroCopyPolicy::Auto,
        write_devices: false,
        trust_sender: false,
        partial: false,
        specials: None,
        qsort: false,
        checksum_seed: None,
        checksum_choice: None,
        min_size: None,
        max_size: None,
        max_alloc: None,
        stop_at: None,
        stop_after: None,
        files_from: None,
        from0: false,
        inplace: false,
        size_only: false,
        modify_window: None,
        numeric_ids: false,
        delete: false,
        remove_source_files: false,
        stats: false,
        ignore_existing: false,
        existing_only: false,
        max_delete: None,
        iconv: None,
        timeout: None,
        reference_directories: Vec::new(),
        info: Vec::new(),
        compress_choice: None,
        compression_level: None,
        log_format: None,
        partial_dir: None,
        delay_updates: false,
        mkpath: false,
        list_only: false,
    };

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        let s = arg.to_string_lossy();

        match s.as_ref() {
            "--sender" => flags.is_sender = true,
            "--receiver" => flags.is_receiver = true,
            "--ignore-errors" => flags.ignore_errors = true,
            "--fsync" => flags.fsync = true,
            "--io-uring" => flags.io_uring_policy = fast_io::IoUringPolicy::Enabled,
            "--no-io-uring" => flags.io_uring_policy = fast_io::IoUringPolicy::Disabled,
            "--zero-copy" => flags.zero_copy_policy = fast_io::ZeroCopyPolicy::Enabled,
            "--no-zero-copy" => flags.zero_copy_policy = fast_io::ZeroCopyPolicy::Disabled,
            "--write-devices" => flags.write_devices = true,
            "--trust-sender" => flags.trust_sender = true,
            // upstream: options.c:2893 - bare --partial (no compact 'P').
            "--partial" => flags.partial = true,
            // upstream: options.c:2760-2765 - --specials / --no-specials convey
            // preserve_specials separately from the compact 'D' (devices) letter.
            "--specials" => flags.specials = Some(true),
            "--no-specials" => flags.specials = Some(false),
            "--qsort" => flags.qsort = true,
            "--from0" => flags.from0 = true,
            "--inplace" => flags.inplace = true,
            "--size-only" => flags.size_only = true,
            // upstream: --numeric-ids is long-form only (options.c:2887-2888)
            "--numeric-ids" => flags.numeric_ids = true,
            // upstream: --delete variants are long-form only (options.c:2818-2827)
            "--delete" | "--delete-before" | "--delete-during" | "--delete-after"
            | "--delete-delay" | "--delete-excluded" => flags.delete = true,
            // upstream: options.c:2964-2965 - --remove-source-files is long-form
            // only. --remove-sent-files is the deprecated alias that still names
            // the same option in `parse_arguments()`.
            "--remove-source-files" | "--remove-sent-files" => {
                flags.remove_source_files = true;
            }
            // upstream: options.c:2838-2839 - --stats forwarded by server_options()
            // when do_stats was set. The server-side flag drives NDX_DEL_STATS
            // emission in the goodbye phase (generator.c:2377,2422).
            "--stats" => flags.stats = true,
            // upstream: options.c:2831 - --ignore-existing sent as long-form arg
            "--ignore-existing" => flags.ignore_existing = true,
            // upstream: options.c:2833 - --existing (--ignore-non-existing) sent as long-form arg
            "--existing" | "--ignore-non-existing" => flags.existing_only = true,
            // upstream: options.c:2800-2805 - non-ZLIB compression algorithms
            // come across the wire as bare long flags. Mapping them to the
            // wire-name strings keeps parity with how the daemon path resolves
            // CompressionAlgorithm in `transfer::run_server_with_handshake`.
            "--new-compress" => flags.compress_choice = Some("zlibx".to_owned()),
            "--old-compress" => flags.compress_choice = Some("zlib".to_owned()),
            // upstream: options.c:2886-2890 - server_options() emits
            // `--partial-dir` and its value as TWO separate argv entries.
            // Consume the next arg verbatim as the directory path.
            "--partial-dir" => {
                if let Some(next) = args.get(idx + 1) {
                    flags.partial_dir = Some(next.clone());
                    idx += 1;
                }
            }
            // upstream: options.c:2891-2892 - emitted alongside --partial-dir.
            "--delay-updates" => flags.delay_updates = true,
            // upstream: options.c:2996-2997 - --mkpath forwarded to the server
            // receiver when the client is the sender (push). --no-mkpath is the
            // negation (options.c:834). Gates dest-arg path creation below.
            "--mkpath" => flags.mkpath = true,
            "--no-mkpath" => flags.mkpath = false,
            // upstream: options.c:2747-2748 - `--list-only` forwarded when the
            // client used the explicit flag (list_only > 1). The receiver lists
            // the flist without writing to the destination.
            "--list-only" => flags.list_only = true,
            // upstream: options.c:2782-2785 - `--msgs2stderr` / `--no-msgs2stderr`
            // control the peer's own diagnostic routing. Recognised here so the
            // flag is consumed rather than treated as a positional path; the
            // server's message routing is handled elsewhere.
            "--msgs2stderr" | "--no-msgs2stderr" => {}
            // upstream: options.c:2197-2199 - `--old-dirs`/`--old-d` set
            // xfer_dirs=4, resolved to recurse=1 plus an appended `- /*/*`
            // filter. server_options() (options.c:2605) never forwards these
            // deprecated flags; a client encodes them as `-r` in the compact
            // flag string and sends `- /*/*` over the wire filter list. Recognise
            // them here only so a stray forward is consumed rather than mistaken
            // for a positional path; they carry no mkpath semantics.
            "--old-dirs" | "--old-d" => {}
            _ => {
                // upstream: options.c::server_options() emits a handful of
                // path-bearing long flags (`--copy-dest`, `--link-dest`,
                // `--compare-dest`, `--files-from`, `--backup-dir`,
                // `--temp-dir`) as two adjacent argv slots via
                // `safe_arg("", value)`. Consume the following slot here so
                // the value lands in the structured field instead of leaking
                // through `parse_value_bearing_flag` and being misclassified
                // as a positional destination path further down.
                if is_two_arg_server_long_flag(s.as_ref()) {
                    let value = args
                        .get(idx + 1)
                        .map(|v| v.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    apply_two_arg_long_flag(s.as_ref(), &value, &mut flags);
                    idx += 2;
                    continue;
                }
                // Accept the joined `--partial-dir=VALUE` form too, even
                // though upstream's server_options() does not emit it - the
                // CLI parser accepts both forms for client-side use, and a
                // forwarder built on a non-upstream client might still send
                // the joined form.
                if let Some(value) = s.strip_prefix("--partial-dir=") {
                    flags.partial_dir = Some(OsString::from(value));
                } else {
                    parse_value_bearing_flag(&s, &mut flags);
                }
            }
        }
        idx += 1;
    }

    flags
}

/// Stores the value of a two-arg long flag (`--flag VALUE`) into the
/// matching field of [`ServerLongFlags`].
///
/// `--backup-dir` and `--temp-dir` are recognised so the value slot does
/// not leak into the positional argument list, but the corresponding
/// fields are not currently consumed by `ServerLongFlags`. Recognising
/// them here is the smallest defence that keeps the alt-dest interop
/// scenario (`--copy-dest /path . dest/`) from mis-mapping the value to
/// the destination root.
///
/// # Upstream Reference
///
/// - `options.c:2807-2808` - `--backup-dir`
/// - `options.c:2926-2927` - `--temp-dir`
/// - `options.c:2939-2940` - `--copy-dest` / `--link-dest` / `--compare-dest`
///   via `alt_dest_opt(0)` + `safe_arg("", basis_dir[i])`
/// - `options.c:2964-2965` - `--files-from`
fn apply_two_arg_long_flag(flag: &str, value: &str, flags: &mut ServerLongFlags) {
    match flag {
        "--compare-dest" => flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            PathBuf::from(value),
        )),
        "--copy-dest" => flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            PathBuf::from(value),
        )),
        "--link-dest" => flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Link,
            PathBuf::from(value),
        )),
        "--files-from" => flags.files_from = Some(value.to_owned()),
        // Values are drained but not currently consumed; recognising the
        // flag here keeps the value out of the positional list.
        "--backup-dir" | "--temp-dir" => {}
        _ => {}
    }
}

/// Parses value-bearing `--flag=value` arguments into `ServerLongFlags`.
fn parse_value_bearing_flag(s: &str, flags: &mut ServerLongFlags) {
    if let Some(value) = s.strip_prefix("--checksum-seed=") {
        flags.checksum_seed = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--checksum-choice=") {
        flags.checksum_choice = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--modify-window=") {
        // upstream: options.c - server_options() emits `--modify-window=%d`.
        flags.modify_window = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--min-size=") {
        flags.min_size = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--max-size=") {
        flags.max_size = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--max-alloc=") {
        flags.max_alloc = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--stop-at=") {
        flags.stop_at = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--stop-after=") {
        flags.stop_after = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--files-from=") {
        flags.files_from = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--max-delete=") {
        flags.max_delete = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--iconv=") {
        // upstream: options.c:2716-2723 - server-side iconv forwarded by client
        flags.iconv = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--timeout=") {
        // upstream: options.c - server_options() emits `--timeout=%d` from io_timeout
        flags.timeout = Some(value.to_owned());
    } else if let Some(value) = s.strip_prefix("--io-uring-depth=") {
        flags.io_uring_depth = Some(value.to_owned());
    // upstream: options.c:2800-2805 - `--compress-choice=ALGO` / `--zc=ALGO`
    // names the negotiated codec when it is not the default CPRES_ZLIB.
    } else if let Some(value) = s
        .strip_prefix("--compress-choice=")
        .or_else(|| s.strip_prefix("--zc="))
    {
        flags.compress_choice = Some(value.to_owned());
    // upstream: options.c:2754-2758 - `--compress-level=%d` carries the
    // explicit compression level so the server codec matches the client.
    } else if let Some(value) = s.strip_prefix("--compress-level=") {
        flags.compression_level = Some(value.to_owned());
    // upstream: options.c:2750-2762 - client forwards --log-format=%i (or %o,
    // %i%I, X) so the server knows whether to generate itemize data.
    } else if let Some(value) = s.strip_prefix("--log-format=") {
        flags.log_format = Some(value.to_owned());
    // upstream: options.c:2928-2931 - server_options() forwards info levels.
    // Capture the raw value so run_server_mode can parse it tolerantly via
    // parse_info_flags_server (mirroring `am_server` in parse_output_words).
    } else if let Some(value) = s.strip_prefix("--info=") {
        flags.info.push(OsString::from(value));
    // upstream: options.c:2915-2923 - reference directory args
    } else if let Some(value) = s.strip_prefix("--compare-dest=") {
        flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            PathBuf::from(value),
        ));
    } else if let Some(value) = s.strip_prefix("--copy-dest=") {
        flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            PathBuf::from(value),
        ));
    } else if let Some(value) = s.strip_prefix("--link-dest=") {
        flags.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Link,
            PathBuf::from(value),
        ));
    }
}

/// Returns `true` when the argument is a known server-mode long flag.
///
/// Used by [`super::parse::parse_server_flag_string_and_args`] to skip long
/// flags when searching for the compact flag string.
pub(super) fn is_known_server_long_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--server"
            | "--sender"
            | "--receiver"
            | "--ignore-errors"
            | "--fsync"
            | "--io-uring"
            | "--no-io-uring"
            | "--zero-copy"
            | "--no-zero-copy"
            | "--write-devices"
            | "--copy-devices"
            | "--trust-sender"
            | "--partial"
            | "--specials"
            | "--no-specials"
            | "--list-only"
            | "--msgs2stderr"
            | "--no-msgs2stderr"
            | "--qsort"
            | "--from0"
            | "--inplace"
            | "--size-only"
            | "--numeric-ids"
            | "--delete"
            | "--delete-before"
            | "--delete-during"
            | "--delete-after"
            | "--delete-delay"
            | "--delete-excluded"
            | "--remove-source-files"
            | "--remove-sent-files"
            | "--copy-unsafe-links"
            | "--safe-links"
            | "--stats"
            | "--ignore-existing"
            | "--existing"
            | "--ignore-non-existing"
            | "--delay-updates"
            | "--partial-dir"
            | "--backup"
            | "--mkpath"
            | "--no-mkpath"
            | "--old-dirs"
            | "--old-d"
    ) || arg == "-s"
        || arg == "--new-compress"
        || arg == "--old-compress"
        || arg.starts_with("--checksum-seed=")
        || arg.starts_with("--checksum-choice=")
        || arg.starts_with("--compress-choice=")
        || arg.starts_with("--compress-level=")
        || arg.starts_with("--zc=")
        || arg.starts_with("--compare-dest=")
        || arg.starts_with("--copy-dest=")
        || arg.starts_with("--link-dest=")
        || arg.starts_with("--modify-window=")
        || arg.starts_with("--min-size=")
        || arg.starts_with("--max-size=")
        || arg.starts_with("--max-alloc=")
        || arg.starts_with("--stop-at=")
        || arg.starts_with("--stop-after=")
        || arg.starts_with("--files-from=")
        || arg.starts_with("--max-delete=")
        || arg.starts_with("--iconv=")
        || arg.starts_with("--timeout=")
        || arg.starts_with("--io-uring-depth=")
        || arg.starts_with("--log-format=")
        || arg.starts_with("--info=")
        || arg.starts_with("--partial-dir=")
}

/// Returns `true` when the argument is a bare server-mode long flag whose
/// value is supplied as the next positional argument.
///
/// Upstream `options.c::server_options()` emits a handful of path-bearing
/// long flags as two argv slots: the flag name and the value, joined later
/// by `safe_arg("", value)`. The split form is used unconditionally for
/// these flags, independent of `protect_args`. Without this awareness the
/// flag name is treated as the first positional path and the value as the
/// second, so the alt-dest interop test (`--copy-dest /path/alt3 . /path/to/`)
/// lands the source files under `$HOME/--copy-dest/` instead of creating
/// the dest root at `/path/to/`.
///
/// `--partial-dir` is handled separately in `parse_server_long_flags`
/// because it predates this helper and keeps its own `idx += 1` branch.
/// All other split-form path flags route through this predicate.
///
/// # Upstream Reference
///
/// - `options.c:2807-2808` - `--backup-dir`
/// - `options.c:2926-2927` - `--temp-dir`
/// - `options.c:2939-2940` - `--copy-dest` / `--link-dest` / `--compare-dest`
///   (via `alt_dest_opt(0)`)
/// - `options.c:2964-2965` - `--files-from`
pub(super) fn is_two_arg_server_long_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--compare-dest"
            | "--copy-dest"
            | "--link-dest"
            | "--backup-dir"
            | "--temp-dir"
            | "--files-from"
    )
}
