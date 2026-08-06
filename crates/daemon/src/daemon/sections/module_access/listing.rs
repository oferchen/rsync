// Module listing - formats and sends the list of available modules to clients.
//
// When a client sends `#list` as its module request, the daemon responds with
// one `%-15s\t%s\n` line per listable module, terminated by `@RSYNCD: EXIT`.
// The MOTD is emitted earlier, right after the greeting.
//
// upstream: clientserver.c:1246-1254 - `rsync_module()` handles the `#list`
// request by iterating `lp_numservices()` and printing each listable module.

/// Sends the list of available modules to a client.
///
/// This responds to a module listing request by sending the names and comments
/// of every listable module. A module appears iff `list = yes` (the default);
/// host access (`hosts allow`/`hosts deny`) is NOT applied here - it is enforced
/// only when the client tries to open a module. A host denied access still sees
/// the module in the listing, matching upstream. The MOTD is emitted earlier,
/// right after the greeting, matching upstream's `exchange_protocols()`
/// (clientserver.c:158-170).
fn respond_with_module_list(
    stream: &mut DaemonStream,
    limiter: &mut Option<BandwidthLimiter>,
    modules: &[ModuleRuntime],
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    // upstream: clientserver.c:1261-1272 send_listing() - the listing loops over
    // every module and prints it iff `lp_list(i)`. No `allow_access()`/hosts
    // check is applied; host filtering happens only in rsync_module() on the
    // module-open path (clientserver.c:728). The listing goes straight to the
    // module names followed by @RSYNCD: EXIT - it does NOT send @RSYNCD: OK
    // first, and the MOTD was already emitted after the greeting.
    for module in modules {
        if !module.listable {
            continue;
        }

        // upstream: clientserver.c:1267 - io_printf(fd, "%-15s\t%s\n", lp_name(i), lp_comment(i));
        let comment = module.comment.as_deref().unwrap_or("");
        let line = format_daemon_module_listing(&module.name, comment);
        write_limited(stream, limiter, line.as_bytes())?;
    }

    messages.write_exit(stream, limiter)?;
    // upstream: the client may close the connection immediately after reading
    // @RSYNCD: EXIT, so a BrokenPipe on flush is expected and harmless.
    match stream.flush() {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e),
    }
}
