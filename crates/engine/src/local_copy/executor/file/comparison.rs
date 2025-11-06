use std::fs;
use std::io::{self, Read};
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::delta::{DeltaSignatureIndex, SignatureLayoutParams, calculate_signature_layout};
use crate::local_copy::LocalCopyError;
use crate::signature::{SignatureAlgorithm, SignatureError, generate_file_signature};

use rsync_checksums::strong::{Md4, Md5, Xxh3, Xxh3_128, Xxh64};
use rsync_meta::MetadataOptions;
use rsync_protocol::ProtocolVersion;

use super::super::super::COPY_BUFFER_SIZE;

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
    pub(crate) options: &'a MetadataOptions,
    pub(crate) size_only: bool,
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
        options,
        size_only,
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

    if size_only {
        return true;
    }

    if options.times() {
        match (source.modified(), destination.modified()) {
            (Ok(src), Ok(dst)) if system_time_within_window(src, dst, modify_window) => {}
            _ => return false,
        }
    } else {
        return false;
    }

    files_match(source_path, destination_path)
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

pub(crate) fn files_match(source: &Path, destination: &Path) -> bool {
    let mut source_file = match fs::File::open(source) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut destination_file = match fs::File::open(destination) {
        Ok(file) => file,
        Err(_) => return false,
    };

    let mut source_buffer = vec![0u8; COPY_BUFFER_SIZE];
    let mut destination_buffer = vec![0u8; COPY_BUFFER_SIZE];

    loop {
        let source_read = match source_file.read(&mut source_buffer) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        let destination_read = match destination_file.read(&mut destination_buffer) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };

        if source_read != destination_read {
            return false;
        }

        if source_read == 0 {
            return true;
        }

        if source_buffer[..source_read] != destination_buffer[..destination_read] {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

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
}

pub(crate) enum StrongHasher {
    Md4(Md4),
    Md5(Md5),
    Xxh64(Xxh64),
    Xxh3(Xxh3),
    Xxh128(Xxh3_128),
}

impl StrongHasher {
    fn new(algorithm: SignatureAlgorithm) -> Self {
        match algorithm {
            SignatureAlgorithm::Md4 => StrongHasher::Md4(Md4::new()),
            SignatureAlgorithm::Md5 => StrongHasher::Md5(Md5::new()),
            SignatureAlgorithm::Xxh64 { seed } => StrongHasher::Xxh64(Xxh64::new(seed)),
            SignatureAlgorithm::Xxh3 { seed } => StrongHasher::Xxh3(Xxh3::new(seed)),
            SignatureAlgorithm::Xxh3_128 { seed } => StrongHasher::Xxh128(Xxh3_128::new(seed)),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            StrongHasher::Md4(state) => state.update(data),
            StrongHasher::Md5(state) => state.update(data),
            StrongHasher::Xxh64(state) => state.update(data),
            StrongHasher::Xxh3(state) => state.update(data),
            StrongHasher::Xxh128(state) => state.update(data),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            StrongHasher::Md4(state) => state.finalize().as_ref().to_vec(),
            StrongHasher::Md5(state) => state.finalize().as_ref().to_vec(),
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
    let mut source_file = fs::File::open(source)?;
    let mut destination_file = fs::File::open(destination)?;

    let mut source_hasher = StrongHasher::new(algorithm);
    let mut destination_hasher = StrongHasher::new(algorithm);

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

        source_hasher.update(&source_buffer[..source_read]);
        destination_hasher.update(&destination_buffer[..destination_read]);
    }

    Ok(source_hasher.finalize() == destination_hasher.finalize())
}
