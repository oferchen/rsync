use super::{SPARSE_WRITE_SIZE, SparseRegion};

/// Detects sparse (zero-filled) regions in data buffers.
///
/// This detector scans data buffers to identify runs of zero bytes that can be
/// efficiently represented as holes in sparse files. It uses optimized scanning
/// with a configurable minimum hole size threshold.
///
/// # Examples
///
/// ```
/// use engine::{SparseDetector, SparseRegion};
///
/// let detector = SparseDetector::new(4096);
/// let data = vec![0xAA; 100];
/// let regions = detector.scan(&data, 0);
///
/// assert_eq!(regions.len(), 1);
/// assert!(matches!(regions[0], SparseRegion::Data { offset: 0, length: 100 }));
/// ```
pub struct SparseDetector {
    min_hole_size: usize,
}

impl SparseDetector {
    /// Creates a new sparse detector with the specified minimum hole size.
    ///
    /// Runs of zeros shorter than `min_hole_size` will not be considered holes
    /// and will be treated as regular data instead. This reduces overhead for
    /// small zero runs while still efficiently handling large sparse regions.
    ///
    /// # Arguments
    ///
    /// * `min_hole_size` - Minimum number of consecutive zero bytes to treat as a hole
    pub const fn new(min_hole_size: usize) -> Self {
        Self { min_hole_size }
    }

    /// Creates a detector with the default threshold matching rsync's behavior.
    pub const fn default_threshold() -> Self {
        Self::new(SPARSE_WRITE_SIZE)
    }

    /// Scans a data buffer and returns a list of sparse regions.
    ///
    /// The buffer is analyzed to identify contiguous runs of zeros (potential holes)
    /// and non-zero data regions. Only zero runs at least `min_hole_size` bytes long
    /// are reported as holes.
    ///
    /// # Arguments
    ///
    /// * `data` - The data buffer to scan
    /// * `base_offset` - The starting offset of this data in the file (used for region offsets)
    ///
    /// # Returns
    ///
    /// A vector of `SparseRegion` entries describing the data and hole regions.
    /// An empty input buffer returns an empty vector.
    pub fn scan(&self, data: &[u8], base_offset: u64) -> Vec<SparseRegion> {
        if data.is_empty() {
            return Vec::new();
        }

        let mut regions = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            let remaining = &data[offset..];
            let zero_run_len = leading_zero_run(remaining);

            if zero_run_len >= self.min_hole_size {
                // Found a significant hole
                regions.push(SparseRegion::Hole {
                    offset: base_offset + offset as u64,
                    length: zero_run_len as u64,
                });
                offset += zero_run_len;
            } else if zero_run_len > 0 {
                // Small zero run - find the next significant zero run or end
                let data_start = offset;
                offset += zero_run_len;

                // Scan for next significant hole or end of buffer
                while offset < data.len() {
                    let segment = &data[offset..];
                    let next_zeros = leading_zero_run(segment);

                    if next_zeros >= self.min_hole_size {
                        // Found next hole, emit data region
                        break;
                    }

                    // Skip this small zero run and any following non-zero data
                    offset += next_zeros;
                    if offset < data.len() {
                        let non_zeros = segment[next_zeros..]
                            .iter()
                            .position(|&b| b == 0)
                            .unwrap_or(segment.len() - next_zeros);
                        offset += non_zeros;
                    }
                }

                // Emit the data region
                regions.push(SparseRegion::Data {
                    offset: base_offset + data_start as u64,
                    length: (offset - data_start) as u64,
                });
            } else {
                // No zeros at start, find first zero or end
                let non_zero_len = remaining
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(remaining.len());

                regions.push(SparseRegion::Data {
                    offset: base_offset + offset as u64,
                    length: non_zero_len as u64,
                });
                offset += non_zero_len;
            }
        }

        regions
    }

    /// Quickly checks if the entire buffer is all zeros.
    ///
    /// This is faster than calling `scan()` when you only need to know whether
    /// the buffer contains any non-zero data.
    ///
    /// # Arguments
    ///
    /// * `data` - The data buffer to check
    ///
    /// # Returns
    ///
    /// `true` if all bytes in the buffer are zero, `false` otherwise.
    /// An empty buffer returns `true`.
    pub fn is_all_zeros(data: &[u8]) -> bool {
        if data.is_empty() {
            return true;
        }
        leading_zero_run(data) == data.len()
    }
}

// ---------------------------------------------------------------------------
// Zero-run detection primitives
// ---------------------------------------------------------------------------

#[inline]
pub(super) fn leading_zero_run(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut iter = bytes.chunks_exact(16);

    for chunk in &mut iter {
        // SAFETY: chunks_exact(16) guarantees exactly 16-byte slices, so try_into cannot fail.
        let chunk: &[u8; 16] = chunk.try_into().expect("chunks_exact guarantees 16 bytes");
        if u128::from_ne_bytes(*chunk) == 0 {
            offset += 16;
            continue;
        }

        let position = chunk.iter().position(|&byte| byte != 0).unwrap_or(16);
        return offset + position;
    }

    offset + leading_zero_run_scalar(iter.remainder())
}

#[inline]
fn leading_zero_run_scalar(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|&&byte| byte == 0).count()
}

#[inline]
pub(super) fn trailing_zero_run(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut iter = bytes.rchunks_exact(16);

    for chunk in &mut iter {
        // SAFETY: rchunks_exact(16) guarantees exactly 16-byte slices, so try_into cannot fail.
        let chunk: &[u8; 16] = chunk.try_into().expect("chunks_exact guarantees 16 bytes");
        if u128::from_ne_bytes(*chunk) == 0 {
            offset += 16;
            continue;
        }

        let trailing = chunk.iter().rev().take_while(|&&byte| byte == 0).count();
        return offset + trailing;
    }

    offset + trailing_zero_run_scalar(iter.remainder())
}

#[inline]
fn trailing_zero_run_scalar(bytes: &[u8]) -> usize {
    bytes.iter().rev().take_while(|&&byte| byte == 0).count()
}
