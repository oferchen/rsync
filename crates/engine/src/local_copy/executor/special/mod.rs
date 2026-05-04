//! Copy logic for FIFOs, devices, and symbolic links.

mod device;
mod fifo;
mod symlink;

pub(crate) use device::copy_device;
pub(crate) use fifo::copy_fifo;
pub(crate) use symlink::{copy_symlink, create_symlink, symlink_target_is_safe};
