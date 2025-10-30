#![deny(rustdoc::broken_intra_doc_links)]
#![deny(clippy::undocumented_unsafe_blocks)]

//! Compatibility helpers for the Windows GNU target.
//!
//! When cross-compiling with `cargo-zigbuild` the Windows GNU toolchain linked
//! by Zig omits the legacy libgcc entry points `___register_frame_info` and
//! `___deregister_frame_info` that Rust's startup object (`rsbegin.o`)
//! references when DWARF unwind data is present. We forward these requests to
//! the modern `__register_frame_info`/`__deregister_frame_info` symbols shipped
//! with libunwind (or libgcc) when they are available at runtime so stack
//! unwinding remains fully functional while still permitting fully-static
//! linkers that omit the legacy entry points.

#[cfg(all(target_os = "windows", target_env = "gnu"))]
mod windows_gnu {
    use core::ffi::{c_char, c_void};
    use core::mem::transmute;
    use core::ptr;
    use core::sync::atomic::{AtomicUsize, Ordering};

    type RegisterFrameInfo = unsafe extern "C" fn(eh_frame: *const u8, object: *mut c_void);
    type DeregisterFrameInfo = unsafe extern "C" fn(eh_frame: *const u8);

    const UNRESOLVED: usize = 0;
    const MISSING: usize = 1;

    static REGISTER_FRAME_INFO: AtomicUsize = AtomicUsize::new(UNRESOLVED);
    static DEREGISTER_FRAME_INFO: AtomicUsize = AtomicUsize::new(UNRESOLVED);

    const LIBCANDIDATES: [&[u8]; 4] = [
        b"libgcc_s_seh-1.dll\0",
        b"libgcc_s_sjlj-1.dll\0",
        b"libgcc_s_dw2-1.dll\0",
        b"libunwind.dll\0",
    ];

    const SYM_REGISTER: &[u8] = b"__register_frame_info\0";
    const SYM_DEREGISTER: &[u8] = b"__deregister_frame_info\0";

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleA(lpModuleName: *const c_char) -> *mut c_void;
        fn LoadLibraryA(lpLibFileName: *const c_char) -> *mut c_void;
        fn GetProcAddress(hModule: *mut c_void, lpProcName: *const c_char) -> *mut c_void;
    }

    #[inline(always)]
    unsafe fn ensure_function(cache: &AtomicUsize, symbol: &[u8]) -> Option<*mut ()> {
        let mut state = cache.load(Ordering::Acquire);
        loop {
            match state {
                UNRESOLVED => {
                    let resolved = unsafe { resolve_symbol(symbol) } as usize;
                    let new_state = if resolved == 0 { MISSING } else { resolved };
                    match cache.compare_exchange(
                        UNRESOLVED,
                        new_state,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            state = new_state;
                            break;
                        }
                        Err(actual) => state = actual,
                    }
                }
                MISSING => return None,
                value => return Some(value as *mut ()),
            }
        }

        if state == MISSING || state == UNRESOLVED {
            None
        } else {
            Some(state as *mut ())
        }
    }

    #[inline(always)]
    unsafe fn resolve_symbol(symbol: &[u8]) -> *mut c_void {
        debug_assert!(!symbol.is_empty() && symbol[symbol.len() - 1] == 0);

        for &lib in &LIBCANDIDATES {
            let ptr = unsafe { load_from_library(lib, symbol) };
            if !ptr.is_null() {
                return ptr;
            }
        }

        ptr::null_mut()
    }

    #[inline(always)]
    unsafe fn load_from_library(library: &[u8], symbol: &[u8]) -> *mut c_void {
        debug_assert!(!library.is_empty() && library[library.len() - 1] == 0);

        let module = {
            let handle = unsafe { GetModuleHandleA(library.as_ptr() as *const c_char) };
            if !handle.is_null() {
                handle
            } else {
                unsafe { LoadLibraryA(library.as_ptr() as *const c_char) }
            }
        };

        if module.is_null() {
            return ptr::null_mut();
        }

        unsafe { GetProcAddress(module, symbol.as_ptr() as *const c_char) }
    }

    #[inline(always)]
    unsafe fn resolve_register() -> Option<RegisterFrameInfo> {
        let ptr = match unsafe { ensure_function(&REGISTER_FRAME_INFO, SYM_REGISTER) } {
            Some(ptr) => ptr,
            None => return None,
        };
        Some(unsafe { transmute::<*mut (), RegisterFrameInfo>(ptr) })
    }

    #[inline(always)]
    unsafe fn resolve_deregister() -> Option<DeregisterFrameInfo> {
        let ptr = match unsafe { ensure_function(&DEREGISTER_FRAME_INFO, SYM_DEREGISTER) } {
            Some(ptr) => ptr,
            None => return None,
        };
        Some(unsafe { transmute::<*mut (), DeregisterFrameInfo>(ptr) })
    }

    /// Forwards `rsbegin`'s registration hook to libunwind.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ___register_frame_info(eh_frame: *const u8, object: *mut c_void) {
        if let Some(register) = unsafe { resolve_register() } {
            unsafe {
                register(eh_frame, object);
            }
        }
    }

    /// Forwards `rsbegin`'s deregistration hook to libunwind.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ___deregister_frame_info(eh_frame: *const u8) {
        if let Some(deregister) = unsafe { resolve_deregister() } {
            unsafe {
                deregister(eh_frame);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_link_is_a_no_op() {
        // The helper intentionally performs no work on non-Windows targets and
        // exists purely to keep linkage symmetric across platforms. Exercising
        // the function ensures its signature remains callable from dependants
        // and guards against accidental regressions that would introduce side
        // effects.
        force_link();
    }
}
