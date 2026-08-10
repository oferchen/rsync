//! Filter list wire protocol encoding and decoding.
//!
//! Implements rsync's filter list exchange so clients can push
//! include/exclude patterns to servers for server-side filtering during file
//! list generation.
//!
//! ## Wire Format
//!
//! Each filter rule is encoded as:
//! 1. **Length** (4-byte little-endian `int32`): bytes for prefix + pattern +
//!    optional trailing `/`.
//! 2. **Prefix** (variable): rule-type character followed by modifier flags
//!    as ASCII.
//! 3. **Pattern** (UTF-8 bytes): the glob pattern.
//! 4. **Trailing `/`** (optional): present when the directory-only flag is
//!    set.
//!
//! The list is terminated by a 4-byte little-endian zero. Upstream uses
//! `write_int()` / `read_int()` here, not the protocol's varint encoding -
//! see `exclude.c:1658 send_filter_list()` and `exclude.c:1675
//! recv_filter_list()`.
//!
//! ## Prefix Format
//!
//! Layout: rule-type character, optional modifier characters, optional
//! trailing space, then the pattern. The leading rule-type character is one
//! of `+`, `-`, `!`, `.`, `:`, `P`, `R` (see `RuleType`). Modifier flags
//! follow:
//!
//! | Char | Meaning | Protocol |
//! |------|---------|----------|
//! | `/` | Anchored pattern | All |
//! | `!` | Negate (no-match-with-this) | All |
//! | `C` | CVS exclude | All |
//! | `n` | No-inherit | All |
//! | `w` | Word-split | All |
//! | `e` | Exclude pattern from merge | All |
//! | `x` | XAttr only | All |
//! | `s` | Apply sender-side | v29+ |
//! | `r` | Apply receiver-side | v29+ |
//! | `p` | Perishable | v30+ |
//! | ` ` | Trailing space separator | All |

mod prefix;
mod wire;

pub use prefix::build_rule_prefix;
#[cfg(feature = "tokio-transfer")]
pub use wire::read_filter_list_async;
pub use wire::{FilterRuleWireFormat, RuleType, read_filter_list, write_filter_list};
