#![deny(rustdoc::broken_intra_doc_links)]
#![deny(clippy::undocumented_unsafe_blocks)]

//! Compatibility helpers for the Windows GNU target.
//!
//! When cross-compiling with `cargo-zigbuild` the Windows GNU toolchain linked
//! by Zig omits the legacy libgcc entry points `___register_frame_info` and
//! `___deregister_frame_info` that Rust's startup object (`rsbegin.o`)
//! references when DWARF unwind data is present. We forward these requests to
//! the modern `__register_frame_info`/`__deregister_frame_info` symbols shipped
//! with libunwind so stack unwinding remains fully functional.

#[cfg(all(target_os = "windows", target_env = "gnu"))]
mod windows_gnu {
    use core::ffi::c_void;

    extern "C" {
        fn __register_frame_info(eh_frame: *const u8, object: *mut c_void);
        fn __deregister_frame_info(eh_frame: *const u8);
    }

    /// Forwards `rsbegin`'s registration hook to libunwind.
    #[no_mangle]
    pub unsafe extern "C" fn ___register_frame_info(eh_frame: *const u8, object: *mut c_void) {
        __register_frame_info(eh_frame, object);
    }

    /// Forwards `rsbegin`'s deregistration hook to libunwind.
    #[no_mangle]
    pub unsafe extern "C" fn ___deregister_frame_info(eh_frame: *const u8) {
        __deregister_frame_info(eh_frame);
    }

    /// No-op helper invoked by dependants to ensure the crate remains linked.
    #[inline(always)]
    pub fn force_link() {}
}

#[cfg(not(all(target_os = "windows", target_env = "gnu")))]
mod not_windows_gnu {
    /// No-op helper invoked by dependants to keep linkage symmetric across targets.
    #[inline(always)]
    pub fn force_link() {}
}

#[cfg(all(target_os = "windows", target_env = "gnu"))]
pub use windows_gnu::force_link;

#[cfg(not(all(target_os = "windows", target_env = "gnu")))]
pub use not_windows_gnu::force_link;
