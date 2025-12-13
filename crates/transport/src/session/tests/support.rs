use super::*;
use crate::binary::local_compatibility_flags;

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

pub(crate) fn binary_handshake_bytes(version: ProtocolVersion) -> Vec<u8> {
    let mut bytes = u32::from(version.as_u8()).to_be_bytes().to_vec();
    if version.uses_binary_negotiation() {
        local_compatibility_flags()
            .encode_to_vec(&mut bytes)
            .expect("compatibility encoding succeeds");
    }
    bytes
}
