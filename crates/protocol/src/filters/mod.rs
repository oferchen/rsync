//! Filter list wire protocol encoding and decoding.
//!
//! This module implements the rsync filter list exchange protocol, which allows
//! clients to send include/exclude patterns to servers for server-side filtering
//! during file list generation.
//!
//! ## Wire Format
//!
//! Each filter rule is encoded as:
//! 1. **Length** (varint): Total bytes for prefix + pattern + optional `/`
//! 2. **Prefix** (variable): Modifier flags as ASCII characters
//! 3. **Pattern** (string): The glob pattern
//! 4. **Trailing `/`** (optional): If DIRECTORY flag set
//!
//! The list is terminated with a 4-byte zero (varint encoding of 0).
//!
//! ## Prefix Format
//!
//! `[+/-/:][/][!][C][n][w][e][x][s][r][p][ ]`
//!
//! | Char | Meaning | Protocol |
//! |------|---------|----------|
//! | `+` | Include rule | All |
//! | `-` | Exclude rule | All |
//! | `:` | Clear rules | All |
//! | `/` | Anchored pattern | All |
//! | `!` | No-match-with-this negates | All |
//! | `C` | CVS exclude | All |
//! | `n` | No-inherit | All |
//! | `w` | Word-split | All |
//! | `e` | Exclude pattern from merge | All |
//! | `x` | XAttr only | All |
//! | `s` | Apply sender-side | v29+ |
//! | `r` | Apply receiver-side | v29+ |
//! | `p` | Perishable | v30+ |
//! | ` ` | Trailing space if needed | All |

mod prefix;
mod wire;

pub use prefix::build_rule_prefix;
pub use wire::{FilterRuleWireFormat, RuleType, read_filter_list, write_filter_list};
