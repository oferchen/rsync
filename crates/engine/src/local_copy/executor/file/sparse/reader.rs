//! Sparse file reader using `SEEK_HOLE`/`SEEK_DATA` for efficient reading.
//!
//! On Linux, uses `lseek(SEEK_HOLE)` and `lseek(SEEK_DATA)` to skip over
//! filesystem holes without reading zero bytes. Falls back to sequential
//! reading with zero-run detection on other platforms.

use std::fs;
use std::io::{self, Seek, SeekFrom};

#[cfg(target_os = "linux")]
use rustix::{fd::AsFd, io::Errno};

use super::{SPARSE_WRITE_SIZE, SparseDetector, SparseRegion};

/// Reads sparse files efficiently using filesystem hole detection.
///
/// On Linux systems with filesystem support, this uses `SEEK_HOLE`/`SEEK_DATA`
/// to efficiently detect existing holes without reading zero-filled data.
/// On other platforms, it falls back to scanning file contents.
///
/// # Platform Support
///
/// - **Linux 3.1+**: Uses `lseek(SEEK_HOLE)` and `lseek(SEEK_DATA)` for efficient hole detection
/// - **Other platforms**: Falls back to reading and scanning file contents
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use engine::{SparseReader, SparseRegion};
///
/// let file = File::open("sparse_file.bin").unwrap();
/// let regions = SparseReader::detect_holes(&file).unwrap();
///
/// for region in regions {
///     match region {
///         SparseRegion::Data { offset, length } => {
///             println!("Data at {}: {} bytes", offset, length);
///         }
///         SparseRegion::Hole { offset, length } => {
///             println!("Hole at {}: {} bytes", offset, length);
///         }
///     }
/// }
/// ```
pub struct SparseReader;

impl SparseReader {
    /// Detects holes in a file using filesystem-specific mechanisms.
    ///
    /// On Linux with SEEK_HOLE/SEEK_DATA support, this efficiently queries the
    /// filesystem for hole locations without reading file contents. On other
    /// platforms, it falls back to reading and scanning the file.
    ///
    /// # Arguments
    ///
    /// * `file` - A reference to the file to analyze
    ///
    /// # Returns
    ///
    /// A vector of `SparseRegion` entries describing data and hole regions in
    /// the file, or an I/O error if the file cannot be read or queried.
    ///
    /// # Platform-Specific Behavior
    ///
    /// - **Linux**: Uses `SEEK_HOLE`/`SEEK_DATA` syscalls for efficient detection
    /// - **Other platforms**: Reads file in chunks and scans for zero runs
    #[cfg(target_os = "linux")]
    pub fn detect_holes(file: &fs::File) -> io::Result<Vec<SparseRegion>> {
        Self::detect_holes_linux(file)
    }

    /// Fallback hole detection for non-Linux platforms.
    #[cfg(not(target_os = "linux"))]
    pub fn detect_holes(file: &fs::File) -> io::Result<Vec<SparseRegion>> {
        Self::detect_holes_fallback(file)
    }

    /// Linux-specific hole detection using SEEK_HOLE and SEEK_DATA.
    #[cfg(target_os = "linux")]
    fn detect_holes_linux(file: &fs::File) -> io::Result<Vec<SparseRegion>> {
        use rustix::fs::SeekFrom as RustixSeekFrom;

        let mut regions = Vec::new();
        let file_size = file.metadata()?.len();

        if file_size == 0 {
            return Ok(regions);
        }

        let fd = file.as_fd();
        let mut pos = 0u64;

        while pos < file_size {
            match rustix::fs::seek(fd, RustixSeekFrom::Data(pos)) {
                Ok(data_start) => {
                    if data_start > pos {
                        regions.push(SparseRegion::Hole {
                            offset: pos,
                            length: data_start - pos,
                        });
                    }

                    match rustix::fs::seek(fd, RustixSeekFrom::Hole(data_start)) {
                        Ok(hole_start) => {
                            if hole_start > data_start {
                                regions.push(SparseRegion::Data {
                                    offset: data_start,
                                    length: hole_start - data_start,
                                });
                            }

                            pos = hole_start;
                        }
                        Err(Errno::NXIO) => {
                            regions.push(SparseRegion::Data {
                                offset: data_start,
                                length: file_size - data_start,
                            });
                            break;
                        }
                        Err(_e) => {
                            return Self::detect_holes_fallback(file);
                        }
                    }
                }
                Err(Errno::NXIO) => {
                    if pos < file_size {
                        regions.push(SparseRegion::Hole {
                            offset: pos,
                            length: file_size - pos,
                        });
                    }
                    break;
                }
                Err(_e) => {
                    return Self::detect_holes_fallback(file);
                }
            }
        }

        Ok(regions)
    }

    /// Fallback hole detection by reading and scanning file contents.
    ///
    /// This is used on platforms without SEEK_HOLE/SEEK_DATA support or when
    /// those operations fail. It reads the file in chunks and uses
    /// `SparseDetector` to identify zero runs.
    fn detect_holes_fallback(file: &fs::File) -> io::Result<Vec<SparseRegion>> {
        use std::io::Read;

        let file_size = file.metadata()?.len();
        if file_size == 0 {
            return Ok(Vec::new());
        }

        let mut file_clone = file.try_clone()?;
        file_clone.seek(SeekFrom::Start(0))?;

        let detector = SparseDetector::new(SPARSE_WRITE_SIZE);
        let mut all_regions = Vec::new();
        let mut buffer = vec![0u8; 1024 * 1024]; // 1MB chunks
        let mut offset = 0u64;

        loop {
            let bytes_read = file_clone.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }

            let chunk_regions = detector.scan(&buffer[..bytes_read], offset);
            all_regions.extend(chunk_regions);
            offset += bytes_read as u64;
        }

        // Coalesce adjacent regions of the same type
        Self::coalesce_regions(&mut all_regions);

        Ok(all_regions)
    }

    /// Coalesces adjacent regions of the same type.
    ///
    /// If two Data regions or two Hole regions are adjacent, they are merged
    /// into a single region to simplify the region list.
    pub(super) fn coalesce_regions(regions: &mut Vec<SparseRegion>) {
        if regions.len() < 2 {
            return;
        }

        let mut write_idx = 0;
        let mut read_idx = 1;

        while read_idx < regions.len() {
            let can_merge = match (regions[write_idx], regions[read_idx]) {
                (
                    SparseRegion::Data {
                        offset: o1,
                        length: l1,
                    },
                    SparseRegion::Data {
                        offset: o2,
                        length: _,
                    },
                ) if o1 + l1 == o2 => true,
                (
                    SparseRegion::Hole {
                        offset: o1,
                        length: l1,
                    },
                    SparseRegion::Hole {
                        offset: o2,
                        length: _,
                    },
                ) if o1 + l1 == o2 => true,
                _ => false,
            };

            if can_merge {
                // Merge regions[read_idx] into regions[write_idx]
                let merged = match (regions[write_idx], regions[read_idx]) {
                    (
                        SparseRegion::Data { offset, length: l1 },
                        SparseRegion::Data { length: l2, .. },
                    ) => SparseRegion::Data {
                        offset,
                        length: l1 + l2,
                    },
                    (
                        SparseRegion::Hole { offset, length: l1 },
                        SparseRegion::Hole { length: l2, .. },
                    ) => SparseRegion::Hole {
                        offset,
                        length: l1 + l2,
                    },
                    _ => unreachable!(),
                };
                regions[write_idx] = merged;
            } else {
                write_idx += 1;
                regions[write_idx] = regions[read_idx];
            }

            read_idx += 1;
        }

        regions.truncate(write_idx + 1);
    }
}
