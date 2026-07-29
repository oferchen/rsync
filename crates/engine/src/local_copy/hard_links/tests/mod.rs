//! Test modules for the `hard_links` decomposition.

mod apply_tracker_tests;
mod hard_link_dispatch_tests;
mod tracker_tests;

#[cfg(unix)]
mod cross_device_tests;
#[cfg(unix)]
mod detection_tests;
#[cfg(unix)]
mod device_inode_tests;
#[cfg(unix)]
mod preservation_tests;
#[cfg(unix)]
mod scale_tests;
