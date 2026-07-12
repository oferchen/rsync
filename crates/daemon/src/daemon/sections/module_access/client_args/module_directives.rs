// Resolution of daemon module directives that depend on the selected module:
// the `charset =` iconv converter and the `incoming`/`outgoing chmod` specs.
/// Resolves the daemon module's `charset =` directive into a
/// [`FilenameConverter`] that mirrors upstream rsync's iconv setup.
///
/// Upstream `clientserver.c:712-716` sets `iconv_opt = lp_charset(i)` and
/// calls `setup_iconv()` whenever the value is non-empty. `setup_iconv()`
/// (`rsync.c:87-140`) then opens two iconv handles:
///
/// - `ic_send = iconv_open(UTF8, charset)` - convert local-charset bytes to
///   UTF-8 wire bytes when sending file lists.
/// - `ic_recv = iconv_open(charset, UTF8)` - convert UTF-8 wire bytes back
///   to the local charset when receiving file lists.
///
/// When the directive includes a comma (`charset = LOCAL,REMOTE`), upstream
/// honours the server side by using the segment after the comma
/// (`rsync.c:118-120`, `am_server` branch). When no comma is present, the
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
/// `encoding_rs`. An unrecognised charset is treated as a soft failure: we
/// log via `tracing` (when enabled) and fall through to identity conversion,
/// matching the lenient behaviour `IconvSetting::resolve_converter` already
/// uses on the client side.
///
/// # Upstream Reference
///
/// - `clientserver.c:712-716` - `iconv_opt = lp_charset(i); setup_iconv();`
/// - `rsync.c:87-140` - `setup_iconv()` opens `ic_send` and `ic_recv`.
/// - `loadparm.c` - `charset` module parameter.
fn resolve_module_charset_converter(charset: Option<&str>) -> Option<FilenameConverter> {
    let raw = charset?.trim();
    if raw.is_empty() {
        return None;
    }

    // upstream: rsync.c:118-120 - on the server side, the segment after the
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

/// Parses the module's `incoming chmod` / `outgoing chmod` directives into the
/// typed [`metadata::ChmodModifiers`] used by the receiver and sender code
/// paths. Returns a human-readable error message when a spec is malformed so
/// the caller can wrap it in an `@ERROR` reply.
///
/// # Upstream Reference
///
/// - `options.c:parse_chmod()` - canonical chmod-spec grammar
/// - `clientserver.c:rsync_module()` - daemon-side arming of `daemon_chmod_modes`
fn parse_daemon_chmod_specs(
    module: &ModuleRuntime,
) -> Result<
    (
        Option<metadata::ChmodModifiers>,
        Option<metadata::ChmodModifiers>,
    ),
    String,
> {
    let incoming = parse_one_chmod_spec("incoming chmod", module.incoming_chmod.as_deref())?;
    let outgoing = parse_one_chmod_spec("outgoing chmod", module.outgoing_chmod.as_deref())?;
    Ok((incoming, outgoing))
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
