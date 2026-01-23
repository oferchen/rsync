mod planner;
mod recursive;
mod support;

#[cfg(feature = "parallel")]
mod parallel_planner;

pub(crate) use recursive::copy_directory_recursive;
pub(crate) use support::{is_device, is_fifo};
