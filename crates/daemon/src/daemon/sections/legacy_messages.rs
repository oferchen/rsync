/// Caches frequently emitted legacy daemon protocol messages to avoid
/// repeated allocations while serving clients.
///
/// The helper implements a small flyweight that retains canonical
/// representations of the `@RSYNCD: OK` and `@RSYNCD: EXIT` responses. Dynamic
/// messages fall back to [`format_legacy_daemon_message`], ensuring formatting
/// parity with upstream rsync without duplicating string construction logic.
///
/// The cache is a process-wide `OnceLock` because the `OK` and `EXIT` payloads
/// never change, so rebuilding them per connection would only allocate.
///
/// upstream: clientserver.c:1152 sends `@RSYNCD: OK\n` once a module request
/// is accepted, and clientserver.c:1385 sends `@RSYNCD: EXIT\n` after a module
/// listing.
#[derive(Debug)]
struct LegacyMessageCache {
    ok: Box<[u8]>,
    exit: Box<[u8]>,
}

impl LegacyMessageCache {
    fn shared() -> &'static Self {
        static SHARED: OnceLock<LegacyMessageCache> = OnceLock::new();
        SHARED.get_or_init(|| {
            let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok)
                .into_boxed_str()
                .into_boxed_bytes();
            let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit)
                .into_boxed_str()
                .into_boxed_bytes();
            Self { ok, exit }
        })
    }

    fn render(&self, message: LegacyDaemonMessage<'_>) -> LegacyMessage<'_> {
        match message {
            LegacyDaemonMessage::Ok => LegacyMessage::Borrowed(&self.ok),
            LegacyDaemonMessage::Exit => LegacyMessage::Borrowed(&self.exit),
            other => LegacyMessage::Owned(format_legacy_daemon_message(other)),
        }
    }

    fn write(
        &self,
        stream: &mut DaemonStream,
        limiter: &mut Option<BandwidthLimiter>,
        message: LegacyDaemonMessage<'_>,
    ) -> io::Result<()> {
        let rendered = self.render(message);
        write_limited(stream, limiter, rendered.as_bytes())
    }

    fn write_ok(
        &self,
        stream: &mut DaemonStream,
        limiter: &mut Option<BandwidthLimiter>,
    ) -> io::Result<()> {
        write_limited(stream, limiter, &self.ok)
    }

    fn write_exit(
        &self,
        stream: &mut DaemonStream,
        limiter: &mut Option<BandwidthLimiter>,
    ) -> io::Result<()> {
        write_limited(stream, limiter, &self.exit)
    }
}

/// Borrowed or owned representation of a formatted legacy daemon message.
#[derive(Debug)]
enum LegacyMessage<'a> {
    Borrowed(&'a [u8]),
    Owned(String),
}

impl LegacyMessage<'_> {
    const fn as_bytes(&self) -> &[u8] {
        match self {
            LegacyMessage::Borrowed(bytes) => bytes,
            LegacyMessage::Owned(text) => text.as_bytes(),
        }
    }
}
