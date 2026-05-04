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

include!("buffered_prefix/errors.rs");
include!("buffered_prefix/reader_support.rs");
include!("buffered_prefix/negotiation_prologue_type_tests.rs");
include!("buffered_prefix/detect_prologue_tests.rs");
include!("buffered_prefix/sniffer_buffer_tests.rs");
include!("buffered_prefix/detector_tests.rs");
include!("detector.rs");
include!("detector_sniffer_properties.rs");
include!("detector_sniffer.rs");
include!("sniffer_read.rs");
include!("sniffer_take.rs");
include!("sniffer_reset.rs");
include!("legacy.rs");
