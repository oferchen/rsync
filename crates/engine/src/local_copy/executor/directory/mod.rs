mod planner;
mod recursive;
mod support;

#[cfg(feature = "parallel")]
mod parallel_checksum;
#[cfg(feature = "parallel")]
mod parallel_planner;

#[cfg(feature = "parallel")]
pub(crate) use parallel_checksum::{
    prefetch_checksums, should_skip_with_prefetched_checksum, ChecksumPrefetchResult, FilePair,
};
pub(crate) use recursive::copy_directory_recursive;
pub(crate) use support::{is_device, is_fifo};
