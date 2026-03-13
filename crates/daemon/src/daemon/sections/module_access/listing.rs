// Module listing - formats and sends the list of available modules to clients.
//
// upstream: clientserver.c:1246-1254

/// Formats a single module listing line using upstream's `%-15s\t%s\n` layout.
///
/// The module name is left-aligned in a 15-character wide field, followed by a
/// tab separator and the comment string, terminated by a newline.
///
/// upstream: clientserver.c:1254 - `io_printf(fd, "%-15s\t%s\n", lp_name(i), lp_comment(i));`
fn format_module_listing_line(name: &str, comment: &str) -> String {
    format!("{name:<15}\t{comment}\n")
}

/// Sends the list of available modules to a client.
///
/// This responds to a module listing request by sending the MOTD (message of the
/// day) lines followed by the names and comments of modules the peer is allowed
/// to access. Only modules marked as listable and that permit the peer's IP
/// address are included in the response.
fn respond_with_module_list(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
    modules: &[ModuleRuntime],
    motd_lines: &[String],
    peer_ip: IpAddr,
    reverse_lookup: bool,
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    // upstream: clientserver.c:1246-1252 - MOTD lines are sent as raw text,
    // not wrapped in @RSYNCD: protocol frames.
    for line in motd_lines {
        write_limited(stream, limiter, line.as_bytes())?;
        write_limited(stream, limiter, b"\n")?;
    }

    // upstream: clientserver.c does NOT send @RSYNCD: OK before the module
    // listing. The listing goes straight from MOTD/capabilities to module
    // names followed by @RSYNCD: EXIT.

    let mut hostname_cache: Option<Option<String>> = None;
    for module in modules {
        if !module.listable {
            continue;
        }

        let peer_host = module_peer_hostname(module, &mut hostname_cache, peer_ip, reverse_lookup);
        if !module.permits(peer_ip, peer_host) {
            continue;
        }

        // upstream: clientserver.c:1254 - io_printf(fd, "%-15s\t%s\n", lp_name(i), lp_comment(i));
        let comment = module.comment.as_deref().unwrap_or("");
        let line = format_module_listing_line(&module.name, comment);
        write_limited(stream, limiter, line.as_bytes())?;
    }

    messages.write_exit(stream, limiter)?;
    stream.flush()
}
