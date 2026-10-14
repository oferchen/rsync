// Resolution of daemon module directives that depend on the selected module:
// the `charset =` iconv converter and the `incoming`/`outgoing chmod` specs.
/// Resolves the daemon module's `charset =` directive into a
/// [`FilenameConverter`] that mirrors upstream rsync's iconv setup.
///
/// Upstream `clientserver.c:rsync_module()` (lines 759-764) sets `iconv_opt = lp_charset(i)` and
/// calls `setup_iconv()` whenever the value is non-empty. `setup_iconv()`
/// (`rsync.c:88-148`) then opens two iconv handles:
///
/// - `ic_send = iconv_open(UTF8, charset)` - convert local-charset bytes to
///   UTF-8 wire bytes when sending file lists.
/// - `ic_recv = iconv_open(charset, UTF8)` - convert UTF-8 wire bytes back
///   to the local charset when receiving file lists.
///
/// When the directive includes a comma (`charset = LOCAL,REMOTE`), this uses
/// the segment after the comma, as upstream's `am_server` branch does
/// (`rsync.c:118-123`). When no comma is present, the
/// whole value is the charset. The literal value `.` and the empty string
/// both resolve to the locale default per `rsync.c:125-126`.
///
/// Our [`FilenameConverter`] models the same direction pair: `local_to_remote`
/// matches `ic_send` (local -> wire UTF-8), and `remote_to_local` matches
/// `ic_recv` (wire UTF-8 -> local). Therefore a daemon-side converter is
/// built with the daemon's local charset on the local side and the literal
/// `"UTF-8"` on the remote (wire) side.
///
/// Returns `None` when the directive is absent, empty, or unrecognised by
/// `encoding_rs`. An unrecognised charset is treated as a soft failure: it is
/// logged via `tracing` (when enabled) and filenames are not transcoded,
/// matching the lenient behaviour `IconvSetting::resolve_converter` uses on
/// the client side. Deviation from upstream: `setup_iconv()` exits with
/// `RERR_UNSUPPORTED` when either `iconv_open()` fails (`rsync.c:131-141`).
///
/// # Upstream Reference
///
/// - `clientserver.c:759-764` - `iconv_opt = lp_charset(i); setup_iconv();`
/// - `rsync.c:88-148` - `setup_iconv()` opens `ic_send` and `ic_recv`.
/// - `daemon-parm.txt:21` - `charset` module parameter.
fn resolve_module_charset_converter(charset: Option<&str>) -> Option<FilenameConverter> {
    let raw = charset?.trim();
    if raw.is_empty() {
        return None;
    }

    // upstream: rsync.c:118-123 - on the server side, the segment after the
    // comma is the effective local charset; the segment before the comma
    // describes the peer's local charset and is irrelevant to the daemon.
    let local_part = match raw.split_once(',') {
        Some((_, remote)) => remote.trim(),
        None => raw,
    };

    // upstream: rsync.c:125-126 - empty or "." means "use locale default".
    // Our converter treats UTF-8 as the locale default, matching
    // `converter_from_locale`.
    if local_part.is_empty() || local_part == "." {
        return Some(FilenameConverter::identity());
    }

    FilenameConverter::new(local_part, "UTF-8")
        .inspect_err(|_error| {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                charset = %local_part,
                error = %_error,
                "module 'charset' directive: unsupported encoding; daemon will not transcode filenames",
            );
        })
        .ok()
}

/// Parses the daemon chmod directive for the direction this session uses.
///
/// Returns the upstream log line when the spec does not parse; the caller logs
/// it and serves the session with no daemon chmod.
///
/// # Upstream Reference
///
/// - `chmod.c:parse_chmod()` - canonical chmod-spec grammar
/// - `clientserver.c:1291-1304` - the sender reads only `outgoing chmod`, the
///   receiver only `incoming chmod`; a bad spec logs `Invalid "<dir>ing chmod"
///   directive: <spec>` and `start_server()` still runs
fn daemon_chmod_for_session(
    module: &ModuleRuntime,
    sender: bool,
) -> Result<Option<metadata::ChmodModifiers>, String> {
    let (directive, spec) = if sender {
        ("outgoing chmod", module.outgoing_chmod.as_deref())
    } else {
        ("incoming chmod", module.incoming_chmod.as_deref())
    };
    parse_one_chmod_spec(directive, spec).map_err(|_| {
        format!(
            "Invalid \"{directive}\" directive: {}",
            spec.unwrap_or_default()
        )
    })
}

fn parse_one_chmod_spec(
    directive: &'static str,
    spec: Option<&str>,
) -> Result<Option<metadata::ChmodModifiers>, String> {
    match spec {
        Some(text) => metadata::ChmodModifiers::parse(text)
            .map(Some)
            .map_err(|err| format!("invalid '{directive}' directive '{text}': {err}")),
        None => Ok(None),
    }
}
