use super::sniffer::map_reserve_error_for_io;
use super::*;
use crate::NegotiationError;
use crate::legacy::{LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN};
use proptest::prelude::*;
use std::{
    collections::{HashSet, TryReserveError},
    error::Error as _,
    io::{self, Cursor, IoSliceMut, Read, Write},
    ptr, slice,
    str::FromStr,
};

include!("buffered_prefix.rs");
include!("detector.rs");
include!("detector_sniffer_properties.rs");
include!("detector_sniffer.rs");
include!("sniffer_read.rs");
include!("sniffer_take.rs");
include!("sniffer_reset.rs");
include!("legacy.rs");
