//! Environment-variable override handling for branding detection.

use std::env;
use std::sync::atomic::{AtomicU8, Ordering};

use super::brand::Brand;
use super::constants::BRAND_OVERRIDE_ENV;

const BRAND_OVERRIDE_UNINITIALIZED: u8 = 0;
const BRAND_OVERRIDE_NONE: u8 = 1;
const BRAND_OVERRIDE_UPSTREAM: u8 = 2;
const BRAND_OVERRIDE_OC: u8 = 3;

static BRAND_OVERRIDE_STATE: AtomicU8 = AtomicU8::new(BRAND_OVERRIDE_UNINITIALIZED);

pub(super) fn brand_override_from_env() -> Option<Brand> {
    let mut state = BRAND_OVERRIDE_STATE.load(Ordering::Acquire);

    loop {
        match state {
            BRAND_OVERRIDE_UNINITIALIZED => {
                let value = read_brand_override_from_env();
                let encoded = encode_brand_override(value);
                match BRAND_OVERRIDE_STATE.compare_exchange(
                    BRAND_OVERRIDE_UNINITIALIZED,
                    encoded,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return value,
                    Err(current) => state = current,
                }
            }
            _ => return decode_brand_override(state),
        }
    }
}

#[cfg(test)]
pub(super) fn reset_brand_override_cache() {
    BRAND_OVERRIDE_STATE.store(BRAND_OVERRIDE_UNINITIALIZED, Ordering::Release);
}

fn read_brand_override_from_env() -> Option<Brand> {
    let value = env::var_os(BRAND_OVERRIDE_ENV)?;
    if value.is_empty() {
        return None;
    }

    let value = value.to_string_lossy();
    value.trim().parse::<Brand>().ok()
}

fn encode_brand_override(value: Option<Brand>) -> u8 {
    match value {
        None => BRAND_OVERRIDE_NONE,
        Some(Brand::Upstream) => BRAND_OVERRIDE_UPSTREAM,
        Some(Brand::Oc) => BRAND_OVERRIDE_OC,
    }
}

fn decode_brand_override(state: u8) -> Option<Brand> {
    match state {
        BRAND_OVERRIDE_NONE => None,
        BRAND_OVERRIDE_UPSTREAM => Some(Brand::Upstream),
        BRAND_OVERRIDE_OC => Some(Brand::Oc),
        _ => {
            debug_assert!(
                state == BRAND_OVERRIDE_UNINITIALIZED,
                "unexpected brand override state {state}",
            );
            None
        }
    }
}
