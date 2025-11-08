use std::fs;
use std::io::{self, Read};
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::delta::{DeltaSignatureIndex, SignatureLayoutParams, calculate_signature_layout};
use crate::local_copy::{COPY_BUFFER_SIZE, LocalCopyError};
use crate::signature::{SignatureAlgorithm, SignatureError, generate_file_signature};

use oc_rsync_checksums::strong::{Md4, Md5, Sha1, Xxh3, Xxh3_128, Xxh64};
use oc_rsync_protocol::ProtocolVersion;

pub(crate) fn destination_is_newer(source: &fs::Metadata, destination: &fs::Metadata) -> bool {
    match (source.modified(), destination.modified()) {
        (Ok(src), Ok(dst)) => dst > src,
        _ => false,
    }
}

pub(crate) fn build_delta_signature(
    destination: &Path,
    metadata: &fs::Metadata,
    block_size_override: Option<NonZeroU32>,
) -> Result<Option<DeltaSignatureIndex>, LocalCopyError> {
    let length = metadata.len();
    if length == 0 {
        return Ok(None);
    }

    let checksum_len = NonZeroU8::new(16).expect("strong checksum length must be non-zero");
    let params = SignatureLayoutParams::new(
        length,
        block_size_override,
        ProtocolVersion::NEWEST,
        checksum_len,
    );
    let layout = match calculate_signature_layout(params) {
        Ok(layout) => layout,
        Err(_) => return Ok(None),
    };

    let signature = match generate_file_signature(
        fs::File::open(destination).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?,
        layout,
        SignatureAlgorithm::Md4,
    ) {
        Ok(signature) => signature,
        Err(SignatureError::Io(error)) => {
            return Err(LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
        Err(_) => return Ok(None),
    };

    match DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4) {
        Some(index) => Ok(Some(index)),
        None => Ok(None),
    }
}

pub(crate) struct CopyComparison<'a> {
    pub(crate) source_path: &'a Path,
    pub(crate) source: &'a fs::Metadata,
    pub(crate) destination_path: &'a Path,
    pub(crate) destination: &'a fs::Metadata,
    pub(crate) size_only: bool,
    pub(crate) ignore_times: bool,
    pub(crate) checksum: bool,
    pub(crate) checksum_algorithm: SignatureAlgorithm,
    pub(crate) modify_window: Duration,
}

pub(crate) fn should_skip_copy(params: CopyComparison<'_>) -> bool {
    let CopyComparison {
        source_path,
        source,
        destination_path,
        destination,
        size_only,
        ignore_times,
        checksum,
        checksum_algorithm,
        modify_window,
    } = params;
    if destination.len() != source.len() {
        return false;
    }

    if checksum {
        return files_checksum_match(source_path, destination_path, checksum_algorithm)
            .unwrap_or(false);
    }

    if ignore_times {
        return false;
    }

    if size_only {
        return true;
    }

    match (source.modified(), destination.modified()) {
        (Ok(src), Ok(dst)) => system_time_within_window(src, dst, modify_window),
        _ => false,
    }
}

pub(crate) fn system_time_within_window(a: SystemTime, b: SystemTime, window: Duration) -> bool {
    if window.is_zero() {
        return a.eq(&b);
    }

    match a.duration_since(b) {
        Ok(diff) => diff <= window,
        Err(_) => matches!(b.duration_since(a), Ok(diff) if diff <= window),
    }
}

enum LockstepCheck {
    Continue,
    Diverged,
}

fn compare_files_lockstep<F>(source: &Path, destination: &Path, mut on_chunk: F) -> io::Result<bool>
where
    F: FnMut(&[u8], &[u8]) -> LockstepCheck,
{
    let mut source_file = fs::File::open(source)?;
    let mut destination_file = fs::File::open(destination)?;
    let mut source_buffer = vec![0u8; COPY_BUFFER_SIZE];
    let mut destination_buffer = vec![0u8; COPY_BUFFER_SIZE];

    loop {
        let source_read = source_file.read(&mut source_buffer)?;
        let destination_read = destination_file.read(&mut destination_buffer)?;

        if source_read != destination_read {
            return Ok(false);
        }

        if source_read == 0 {
            break;
        }

        match on_chunk(
            &source_buffer[..source_read],
            &destination_buffer[..destination_read],
        ) {
            LockstepCheck::Continue => {}
            LockstepCheck::Diverged => return Ok(false),
        }
    }

    Ok(true)
}

pub(crate) enum StrongHasher {
    Md4(Md4),
    Md5(Md5),
    Sha1(Sha1),
    Xxh64(Xxh64),
    Xxh3(Xxh3),
    Xxh128(Xxh3_128),
}

impl StrongHasher {
    fn new(algorithm: SignatureAlgorithm) -> Self {
        match algorithm {
            SignatureAlgorithm::Md4 => StrongHasher::Md4(Md4::new()),
            SignatureAlgorithm::Md5 => StrongHasher::Md5(Md5::new()),
            SignatureAlgorithm::Sha1 => StrongHasher::Sha1(Sha1::new()),
            SignatureAlgorithm::Xxh64 { seed } => StrongHasher::Xxh64(Xxh64::new(seed)),
            SignatureAlgorithm::Xxh3 { seed } => StrongHasher::Xxh3(Xxh3::new(seed)),
            SignatureAlgorithm::Xxh3_128 { seed } => StrongHasher::Xxh128(Xxh3_128::new(seed)),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            StrongHasher::Md4(state) => state.update(data),
            StrongHasher::Md5(state) => state.update(data),
            StrongHasher::Sha1(state) => state.update(data),
            StrongHasher::Xxh64(state) => state.update(data),
            StrongHasher::Xxh3(state) => state.update(data),
            StrongHasher::Xxh128(state) => state.update(data),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            StrongHasher::Md4(state) => state.finalize().as_ref().to_vec(),
            StrongHasher::Md5(state) => state.finalize().as_ref().to_vec(),
            StrongHasher::Sha1(state) => state.finalize().as_ref().to_vec(),
            StrongHasher::Xxh64(state) => state.finalize().as_ref().to_vec(),
            StrongHasher::Xxh3(state) => state.finalize().as_ref().to_vec(),
            StrongHasher::Xxh128(state) => state.finalize().as_ref().to_vec(),
        }
    }
}

pub(crate) fn files_checksum_match(
    source: &Path,
    destination: &Path,
    algorithm: SignatureAlgorithm,
) -> io::Result<bool> {
    let mut source_hasher = StrongHasher::new(algorithm);
    let mut destination_hasher = StrongHasher::new(algorithm);

    let identical = compare_files_lockstep(source, destination, |src_chunk, dst_chunk| {
        if src_chunk != dst_chunk {
            LockstepCheck::Diverged
        } else {
            source_hasher.update(src_chunk);
            destination_hasher.update(dst_chunk);
            LockstepCheck::Continue
        }
    })?;

    if !identical {
        return Ok(false);
    }

    Ok(source_hasher.finalize() == destination_hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    use filetime::{FileTime, set_file_mtime};

    #[test]
    fn build_delta_signature_honours_block_size_override() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("data.bin");
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(&vec![0u8; 16384]).expect("write data");
        drop(file);

        let metadata = fs::metadata(&path).expect("metadata");
        let override_size = NonZeroU32::new(2048).unwrap();
        let index = build_delta_signature(&path, &metadata, Some(override_size))
            .expect("signature")
            .expect("index");

        assert_eq!(index.block_length(), override_size.get() as usize);
    }

    #[test]
    fn should_skip_copy_skips_when_metadata_matches() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, b"fresh").expect("write source");
        fs::write(&destination, b"stale").expect("write destination");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let mtime = FileTime::from_system_time(source_meta.modified().expect("source mtime"));
        set_file_mtime(&destination, mtime).expect("set destination mtime");

        let dest_meta = fs::metadata(&destination).expect("dest metadata");
        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &destination,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md5,
            modify_window: Duration::ZERO,
        };

        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_accepts_identical_content_with_identical_timestamps() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, b"fresh").expect("write source");
        fs::write(&destination, b"fresh").expect("write destination");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let mtime = FileTime::from_system_time(source_meta.modified().expect("source mtime"));
        set_file_mtime(&destination, mtime).expect("set destination mtime");

        let dest_meta = fs::metadata(&destination).expect("dest metadata");
        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &destination,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md5,
            modify_window: Duration::ZERO,
        };

        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_respects_ignore_times_even_with_size_only() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, b"fresh").expect("write source");
        fs::write(&destination, b"stale").expect("write destination");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let mtime = FileTime::from_system_time(source_meta.modified().expect("source mtime"));
        set_file_mtime(&destination, mtime).expect("set destination mtime");

        let dest_meta = fs::metadata(&destination).expect("dest metadata");
        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &destination,
            destination: &dest_meta,
            size_only: true,
            ignore_times: true,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md5,
            modify_window: Duration::ZERO,
        };

        assert!(!should_skip_copy(comparison));
    }
}
