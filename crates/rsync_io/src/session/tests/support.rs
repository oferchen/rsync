use super::*;

#[derive(Clone, Debug)]
pub(crate) struct MemoryTransport {
    reader: Cursor<Vec<u8>>,
    writes: Vec<u8>,
    flushes: usize,
}

impl MemoryTransport {
    pub(crate) fn new(input: &[u8]) -> Self {
        Self {
            reader: Cursor::new(input.to_vec()),
            writes: Vec::new(),
            flushes: 0,
        }
    }

    pub(crate) fn writes(&self) -> &[u8] {
        &self.writes
    }

    pub(crate) fn flushes(&self) -> usize {
        self.flushes
    }
}

impl Read for MemoryTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for MemoryTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct InstrumentedTransport {
    inner: MemoryTransport,
}

impl InstrumentedTransport {
    pub(crate) fn new(inner: MemoryTransport) -> Self {
        Self { inner }
    }

    pub(crate) fn writes(&self) -> &[u8] {
        self.inner.writes()
    }

    pub(crate) fn flushes(&self) -> usize {
        self.inner.flushes()
    }
}

impl Read for InstrumentedTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for InstrumentedTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(crate) fn binary_handshake_bytes(version: ProtocolVersion) -> [u8; 4] {
    u32::from(version.as_u8()).to_le_bytes()
}

/// The `@RSYNCD:` line the client sends once the legacy handshake completes.
///
/// The greeting's exact contents - in particular that the advertised digest list
/// is this build's own and not the server's - are pinned by literal-byte
/// assertions in `daemon::negotiate`'s unit tests. Here the line is incidental:
/// these tests check that it survives the stream wrappers intact, so they name it
/// rather than restate it.
pub(crate) fn client_greeting(version: u8) -> Vec<u8> {
    format!(
        "@RSYNCD: {version}.0 {}\n",
        protocol::daemon_auth_digest_list()
    )
    .into_bytes()
}
