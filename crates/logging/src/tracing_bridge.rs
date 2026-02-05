//! crates/logging/src/tracing_bridge.rs
//! Bridge between the tracing crate and rsync's verbosity system.
//!
//! This module provides a custom tracing subscriber layer that maps tracing events
//! to rsync's info and debug flag system. It enables using standard Rust tracing
//! macros (trace!, debug!, info!, warn!, error!) while maintaining compatibility
//! with rsync's verbosity levels and debug flags.
//!
//! # Architecture
//!
//! - [`RsyncLayer`]: A tracing-subscriber layer that filters and processes events
//! - Events are mapped to rsync info/debug flags based on target and metadata
//! - Verbosity configuration is consulted to determine if events should be recorded
//!
//! # Usage
//!
//! ```rust,ignore
//! use logging::{VerbosityConfig, init_tracing};
//!
//! // Initialize tracing with rsync verbosity config
//! let config = VerbosityConfig::from_verbose_level(2);
//! init_tracing(config);
//!
//! // Now use standard tracing macros
//! tracing::info!(target: "rsync::copy", "copying file");
//! tracing::debug!(target: "rsync::delta", "computing delta");
//! ```

use super::config::VerbosityConfig;
use super::levels::{DebugFlag, InfoFlag};
use super::thread_local::{debug_gte, emit_debug, emit_info, info_gte};
use tracing::{Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// A tracing layer that bridges tracing events to rsync's verbosity system.
///
/// This layer intercepts tracing events and maps them to appropriate rsync
/// info or debug flags based on the event's target, level, and metadata.
pub struct RsyncLayer {
    /// Cached verbosity config (not used for filtering, just for context)
    _config: VerbosityConfig,
}

impl RsyncLayer {
    /// Create a new RsyncLayer with the given verbosity configuration.
    #[must_use]
    pub const fn new(config: VerbosityConfig) -> Self {
        Self { _config: config }
    }

    /// Map a tracing target to an rsync info flag.
    fn target_to_info_flag(target: &str) -> Option<InfoFlag> {
        // Match against rsync module paths - look for :: separator or exact word match
        match target {
            t if t.contains("::copy") || t == "copy" => Some(InfoFlag::Copy),
            t if t.contains("::del") || t.contains("::delete") || t == "del" || t == "delete" => {
                Some(InfoFlag::Del)
            }
            t if t.contains("::flist")
                || t.contains("::file_list")
                || t == "flist"
                || t == "file_list" =>
            {
                Some(InfoFlag::Flist)
            }
            t if t.contains("::misc") || t == "misc" => Some(InfoFlag::Misc),
            t if t.contains("::mount") || t == "mount" => Some(InfoFlag::Mount),
            t if t.contains("::name") || t == "name" => Some(InfoFlag::Name),
            t if t.contains("::backup") || t == "backup" => Some(InfoFlag::Backup),
            t if t.contains("::remove") || t == "remove" => Some(InfoFlag::Remove),
            t if t.contains("::skip") || t == "skip" => Some(InfoFlag::Skip),
            t if t.contains("::stats") || t == "stats" => Some(InfoFlag::Stats),
            t if t.contains("::symsafe") || t == "symsafe" => Some(InfoFlag::Symsafe),
            t if t.contains("::nonreg") || t == "nonreg" => Some(InfoFlag::Nonreg),
            t if t.contains("::progress") || t == "progress" => Some(InfoFlag::Progress),
            _ => None,
        }
    }

    /// Map a tracing target to an rsync debug flag.
    fn target_to_debug_flag(target: &str) -> Option<DebugFlag> {
        // Match against rsync module paths - look for :: separator or exact word match
        // This avoids false positives like "unknown" matching "own"
        match target {
            t if t.contains("::acl") || t == "acl" => Some(DebugFlag::Acl),
            t if t.contains("::backup") || t == "backup" => Some(DebugFlag::Backup),
            t if t.contains("::bind") || t == "bind" => Some(DebugFlag::Bind),
            t if t.contains("::chdir") || t == "chdir" => Some(DebugFlag::Chdir),
            t if t.contains("::connect") || t == "connect" => Some(DebugFlag::Connect),
            t if t.contains("::cmd") || t == "cmd" => Some(DebugFlag::Cmd),
            // Check deltasum before del to avoid false matches
            t if t.contains("::deltasum")
                || t.contains("::delta")
                || t == "deltasum"
                || t == "delta" =>
            {
                Some(DebugFlag::Deltasum)
            }
            t if t.contains("::del") || t.contains("::delete") || t == "del" || t == "delete" => {
                Some(DebugFlag::Del)
            }
            t if t.contains("::dup") || t == "dup" => Some(DebugFlag::Dup),
            t if t.contains("::exit") || t == "exit" => Some(DebugFlag::Exit),
            t if t.contains("::filter") || t == "filter" => Some(DebugFlag::Filter),
            t if t.contains("::flist")
                || t.contains("::file_list")
                || t == "flist"
                || t == "file_list" =>
            {
                Some(DebugFlag::Flist)
            }
            t if t.contains("::fuzzy") || t == "fuzzy" => Some(DebugFlag::Fuzzy),
            t if t.contains("::genr")
                || t.contains("::generator")
                || t == "genr"
                || t == "generator" =>
            {
                Some(DebugFlag::Genr)
            }
            t if t.contains("::hash") || t == "hash" => Some(DebugFlag::Hash),
            t if t.contains("::hlink")
                || t.contains("::hardlink")
                || t == "hlink"
                || t == "hardlink" =>
            {
                Some(DebugFlag::Hlink)
            }
            t if t.contains("::iconv") || t == "iconv" => Some(DebugFlag::Iconv),
            t if t.contains("::io") || t == "io" => Some(DebugFlag::Io),
            t if t.contains("::nstr") || t == "nstr" => Some(DebugFlag::Nstr),
            t if t.contains("::own")
                || t.contains("::ownership")
                || t == "own"
                || t == "ownership" =>
            {
                Some(DebugFlag::Own)
            }
            t if t.contains("::proto")
                || t.contains("::protocol")
                || t == "proto"
                || t == "protocol" =>
            {
                Some(DebugFlag::Proto)
            }
            t if t.contains("::recv")
                || t.contains("::receiver")
                || t == "recv"
                || t == "receiver" =>
            {
                Some(DebugFlag::Recv)
            }
            t if t.contains("::send") || t.contains("::sender") || t == "send" || t == "sender" => {
                Some(DebugFlag::Send)
            }
            t if t.contains("::time") || t == "time" => Some(DebugFlag::Time),
            _ => None,
        }
    }

    /// Map a tracing level to a verbosity level.
    const fn level_to_verbosity_level(level: &Level) -> u8 {
        match *level {
            Level::ERROR => 1,
            Level::WARN => 1,
            Level::INFO => 1,
            Level::DEBUG => 2,
            Level::TRACE => 3,
        }
    }
}

impl<S> Layer<S> for RsyncLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let target = metadata.target();
        let level = metadata.level();
        let verbosity_level = Self::level_to_verbosity_level(level);

        // Try to map to debug flag first (more specific)
        if let Some(debug_flag) = Self::target_to_debug_flag(target) {
            if debug_gte(debug_flag, verbosity_level) {
                // Collect the message from the event
                let mut visitor = MessageVisitor::default();
                event.record(&mut visitor);
                if let Some(message) = visitor.message {
                    emit_debug(debug_flag, verbosity_level, message);
                }
            }
            return;
        }

        // Fall back to info flag
        if let Some(info_flag) = Self::target_to_info_flag(target) {
            if info_gte(info_flag, verbosity_level) {
                let mut visitor = MessageVisitor::default();
                event.record(&mut visitor);
                if let Some(message) = visitor.message {
                    emit_info(info_flag, verbosity_level, message);
                }
            }
        }
    }
}

/// Visitor to extract message from tracing event.
#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
        }
    }
}

/// Initialize tracing with rsync verbosity configuration.
///
/// This sets up a tracing subscriber that bridges tracing events to rsync's
/// info/debug flag system. The verbosity configuration determines which events
/// are actually processed.
///
/// # Example
///
/// ```rust,ignore
/// use logging::{VerbosityConfig, init_tracing};
///
/// let config = VerbosityConfig::from_verbose_level(2);
/// init_tracing(config);
///
/// // Now tracing macros work with rsync verbosity
/// tracing::info!(target: "rsync::copy", "file copied");
/// ```
pub fn init_tracing(config: VerbosityConfig) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // Also initialize the thread-local verbosity config
    super::thread_local::init(config.clone());

    let layer = RsyncLayer::new(config);

    tracing_subscriber::registry().with(layer).init();
}

/// Initialize tracing with a custom filter in addition to rsync verbosity.
///
/// This allows combining rsync's verbosity system with standard tracing filters
/// for more fine-grained control.
///
/// # Example
///
/// ```rust,ignore
/// use logging::{VerbosityConfig, init_tracing_with_filter};
/// use tracing_subscriber::EnvFilter;
///
/// let config = VerbosityConfig::from_verbose_level(2);
/// let filter = EnvFilter::from_default_env();
/// init_tracing_with_filter(config, filter);
/// ```
pub fn init_tracing_with_filter<F>(config: VerbosityConfig, filter: F)
where
    F: Layer<tracing_subscriber::Registry> + Send + Sync + 'static,
{
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    super::thread_local::init(config.clone());

    let layer = RsyncLayer::new(config);

    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_to_info_flag() {
        assert_eq!(
            RsyncLayer::target_to_info_flag("rsync::copy"),
            Some(InfoFlag::Copy)
        );
        assert_eq!(
            RsyncLayer::target_to_info_flag("rsync::delete"),
            Some(InfoFlag::Del)
        );
        assert_eq!(
            RsyncLayer::target_to_info_flag("rsync::flist"),
            Some(InfoFlag::Flist)
        );
        assert_eq!(
            RsyncLayer::target_to_info_flag("rsync::stats"),
            Some(InfoFlag::Stats)
        );
        assert_eq!(RsyncLayer::target_to_info_flag("unknown"), None);
    }

    #[test]
    fn test_target_to_debug_flag() {
        assert_eq!(
            RsyncLayer::target_to_debug_flag("rsync::delta"),
            Some(DebugFlag::Deltasum)
        );
        assert_eq!(
            RsyncLayer::target_to_debug_flag("rsync::receiver"),
            Some(DebugFlag::Recv)
        );
        assert_eq!(
            RsyncLayer::target_to_debug_flag("rsync::protocol"),
            Some(DebugFlag::Proto)
        );
        assert_eq!(
            RsyncLayer::target_to_debug_flag("rsync::io"),
            Some(DebugFlag::Io)
        );
        assert_eq!(RsyncLayer::target_to_debug_flag("unknown"), None);
    }

    #[test]
    fn test_level_to_verbosity_level() {
        assert_eq!(RsyncLayer::level_to_verbosity_level(&Level::ERROR), 1);
        assert_eq!(RsyncLayer::level_to_verbosity_level(&Level::WARN), 1);
        assert_eq!(RsyncLayer::level_to_verbosity_level(&Level::INFO), 1);
        assert_eq!(RsyncLayer::level_to_verbosity_level(&Level::DEBUG), 2);
        assert_eq!(RsyncLayer::level_to_verbosity_level(&Level::TRACE), 3);
    }
}

#[cfg(test)]
#[path = "tracing_bridge_tests.rs"]
mod integration_tests;
