struct InterruptedOnceReader {
    inner: Cursor<Vec<u8>>,
    interrupted: bool,
}

impl InterruptedOnceReader {
    fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
            interrupted: false,
        }
    }

    fn was_interrupted(&self) -> bool {
        self.interrupted
    }

    fn into_inner(self) -> Cursor<Vec<u8>> {
        self.inner
    }
}

impl Read for InterruptedOnceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "simulated EINTR during negotiation sniff",
            ));
        }

        self.inner.read(buf)
    }
}

struct RecordingReader {
    inner: Cursor<Vec<u8>>,
    calls: Vec<usize>,
}

impl RecordingReader {
    fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
            calls: Vec::new(),
        }
    }

    fn calls(&self) -> &[usize] {
        &self.calls
    }
}

impl Read for RecordingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !buf.is_empty() {
            self.calls.push(buf.len());
        }

        self.inner.read(buf)
    }
}

struct ChunkedReader {
    inner: Cursor<Vec<u8>>,
    chunk: usize,
}

impl ChunkedReader {
    fn new(data: Vec<u8>, chunk: usize) -> Self {
        assert!(chunk > 0, "chunk size must be non-zero to make progress");
        Self {
            inner: Cursor::new(data),
            chunk,
        }
    }

    fn into_inner(self) -> Cursor<Vec<u8>> {
        self.inner
    }
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let limit = buf.len().min(self.chunk);
        self.inner.read(&mut buf[..limit])
    }
}
