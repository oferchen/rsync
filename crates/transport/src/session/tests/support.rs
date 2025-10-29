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

pub(crate) fn binary_handshake_bytes(version: ProtocolVersion) -> [u8; 4] {
    u32::from(version.as_u8()).to_be_bytes()
}
