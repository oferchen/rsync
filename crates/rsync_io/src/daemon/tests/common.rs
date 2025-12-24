use std::io::{self, Cursor, Read, Write};

#[derive(Clone, Debug)]
pub(super) struct MemoryTransport {
    reader: Cursor<Vec<u8>>,
    written: Vec<u8>,
    flushes: usize,
}

impl MemoryTransport {
    pub(super) fn new(input: &[u8]) -> Self {
        Self {
            reader: Cursor::new(input.to_vec()),
            written: Vec::new(),
            flushes: 0,
        }
    }

    pub(super) fn written(&self) -> &[u8] {
        &self.written
    }

    pub(super) fn flushes(&self) -> usize {
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
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct InstrumentedTransport {
    inner: MemoryTransport,
    observed_writes: Vec<u8>,
    flushes: usize,
}

impl InstrumentedTransport {
    pub(super) fn new(inner: MemoryTransport) -> Self {
        Self {
            inner,
            observed_writes: Vec::new(),
            flushes: 0,
        }
    }

    pub(super) fn writes(&self) -> &[u8] {
        &self.observed_writes
    }

    pub(super) fn flushes(&self) -> usize {
        self.flushes
    }

    pub(super) fn into_inner(self) -> MemoryTransport {
        self.inner
    }
}

impl Read for InstrumentedTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for InstrumentedTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.observed_writes.extend_from_slice(buf);
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        self.inner.flush()
    }
}
