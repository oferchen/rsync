//! crates/logging/src/thread_local.rs
//! Thread-local storage for verbosity configuration and event collection.

use super::config::VerbosityConfig;
use super::levels::{DebugFlag, InfoFlag};
use std::cell::RefCell;

thread_local! {
    static VERBOSITY: RefCell<VerbosityConfig> = RefCell::new(VerbosityConfig::default());
    #[allow(clippy::missing_const_for_thread_local)]
    static EVENTS: RefCell<Vec<DiagnosticEvent>> = RefCell::new(Vec::new());
}

/// Diagnostic event collected during execution.
#[derive(Clone, Debug)]
pub enum DiagnosticEvent {
    /// Info-level diagnostic event.
    Info {
        /// The info flag category.
        flag: InfoFlag,
        /// The verbosity level.
        level: u8,
        /// The diagnostic message.
        message: String,
    },
    /// Debug-level diagnostic event.
    Debug {
        /// The debug flag category.
        flag: DebugFlag,
        /// The verbosity level.
        level: u8,
        /// The diagnostic message.
        message: String,
    },
}

/// Initialize verbosity configuration for the current thread.
pub fn init(config: VerbosityConfig) {
    VERBOSITY.with(|v| {
        *v.borrow_mut() = config;
    });
}

/// Check if the info flag is at or above the specified level.
pub fn info_gte(flag: InfoFlag, level: u8) -> bool {
    VERBOSITY.with(|v| v.borrow().info.get(flag) >= level)
}

/// Check if the debug flag is at or above the specified level.
pub fn debug_gte(flag: DebugFlag, level: u8) -> bool {
    VERBOSITY.with(|v| v.borrow().debug.get(flag) >= level)
}

/// Emit an info diagnostic event.
pub fn emit_info(flag: InfoFlag, level: u8, message: String) {
    EVENTS.with(|e| {
        e.borrow_mut().push(DiagnosticEvent::Info {
            flag,
            level,
            message,
        });
    });
}

/// Emit a debug diagnostic event.
pub fn emit_debug(flag: DebugFlag, level: u8, message: String) {
    EVENTS.with(|e| {
        e.borrow_mut().push(DiagnosticEvent::Debug {
            flag,
            level,
            message,
        });
    });
}

/// Drain all collected events, clearing the internal buffer.
pub fn drain_events() -> Vec<DiagnosticEvent> {
    EVENTS.with(|e| e.borrow_mut().drain(..).collect())
}

/// Apply an info flag token to the current configuration.
pub fn apply_info_flag(token: &str) -> Result<(), String> {
    VERBOSITY.with(|v| v.borrow_mut().apply_info_flag(token))
}

/// Apply a debug flag token to the current configuration.
pub fn apply_debug_flag(token: &str) -> Result<(), String> {
    VERBOSITY.with(|v| v.borrow_mut().apply_debug_flag(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_and_check() {
        let mut config = VerbosityConfig::default();
        config.info.copy = 2;
        config.debug.recv = 3;

        init(config);

        assert!(info_gte(InfoFlag::Copy, 1));
        assert!(info_gte(InfoFlag::Copy, 2));
        assert!(!info_gte(InfoFlag::Copy, 3));

        assert!(debug_gte(DebugFlag::Recv, 1));
        assert!(debug_gte(DebugFlag::Recv, 3));
        assert!(!debug_gte(DebugFlag::Recv, 4));
    }

    #[test]
    fn test_emit_and_drain() {
        init(VerbosityConfig::default());

        emit_info(InfoFlag::Copy, 1, "test info".to_string());
        emit_debug(DebugFlag::Recv, 2, "test debug".to_string());

        let events = drain_events();
        assert_eq!(events.len(), 2);

        match &events[0] {
            DiagnosticEvent::Info {
                flag,
                level,
                message,
            } => {
                assert_eq!(*flag, InfoFlag::Copy);
                assert_eq!(*level, 1);
                assert_eq!(message, "test info");
            }
            _ => panic!("expected info event"),
        }

        match &events[1] {
            DiagnosticEvent::Debug {
                flag,
                level,
                message,
            } => {
                assert_eq!(*flag, DebugFlag::Recv);
                assert_eq!(*level, 2);
                assert_eq!(message, "test debug");
            }
            _ => panic!("expected debug event"),
        }

        // Events should be drained
        assert_eq!(drain_events().len(), 0);
    }

    #[test]
    fn diagnostic_event_clone() {
        let info_event = DiagnosticEvent::Info {
            flag: InfoFlag::Copy,
            level: 1,
            message: "test".to_string(),
        };
        let cloned = info_event.clone();
        match cloned {
            DiagnosticEvent::Info {
                flag,
                level,
                message,
            } => {
                assert_eq!(flag, InfoFlag::Copy);
                assert_eq!(level, 1);
                assert_eq!(message, "test");
            }
            _ => panic!("expected info event"),
        }

        let debug_event = DiagnosticEvent::Debug {
            flag: DebugFlag::Bind,
            level: 2,
            message: "debug".to_string(),
        };
        let cloned = debug_event.clone();
        match cloned {
            DiagnosticEvent::Debug {
                flag,
                level,
                message,
            } => {
                assert_eq!(flag, DebugFlag::Bind);
                assert_eq!(level, 2);
                assert_eq!(message, "debug");
            }
            _ => panic!("expected debug event"),
        }
    }

    #[test]
    fn diagnostic_event_debug() {
        let info_event = DiagnosticEvent::Info {
            flag: InfoFlag::Del,
            level: 3,
            message: "delete".to_string(),
        };
        let debug = format!("{info_event:?}");
        assert!(debug.contains("Info"));
        assert!(debug.contains("Del"));

        let debug_event = DiagnosticEvent::Debug {
            flag: DebugFlag::Io,
            level: 4,
            message: "io debug".to_string(),
        };
        let debug = format!("{debug_event:?}");
        assert!(debug.contains("Debug"));
        assert!(debug.contains("Io"));
    }

    #[test]
    fn apply_info_flag_valid_token() {
        init(VerbosityConfig::default());
        let result = apply_info_flag("copy2");
        assert!(result.is_ok());
        assert!(info_gte(InfoFlag::Copy, 2));
    }

    #[test]
    fn apply_info_flag_invalid_token() {
        init(VerbosityConfig::default());
        let result = apply_info_flag("invalid_flag");
        assert!(result.is_err());
    }

    #[test]
    fn apply_debug_flag_valid_token() {
        init(VerbosityConfig::default());
        let result = apply_debug_flag("io3");
        assert!(result.is_ok());
        assert!(debug_gte(DebugFlag::Io, 3));
    }

    #[test]
    fn apply_debug_flag_invalid_token() {
        init(VerbosityConfig::default());
        let result = apply_debug_flag("not_a_flag");
        assert!(result.is_err());
    }

    #[test]
    fn info_gte_default_config() {
        init(VerbosityConfig::default());
        // Default should be 0 for all flags
        assert!(!info_gte(InfoFlag::Backup, 1));
        assert!(info_gte(InfoFlag::Backup, 0));
    }

    #[test]
    fn debug_gte_default_config() {
        init(VerbosityConfig::default());
        // Default should be 0 for all flags
        assert!(!debug_gte(DebugFlag::Acl, 1));
        assert!(debug_gte(DebugFlag::Acl, 0));
    }

    #[test]
    fn multiple_events_ordering() {
        init(VerbosityConfig::default());
        drain_events(); // Clear any existing

        emit_info(InfoFlag::Copy, 1, "first".to_string());
        emit_info(InfoFlag::Del, 2, "second".to_string());
        emit_debug(DebugFlag::Send, 3, "third".to_string());

        let events = drain_events();
        assert_eq!(events.len(), 3);

        // Events should be in order
        match &events[0] {
            DiagnosticEvent::Info { message, .. } => assert_eq!(message, "first"),
            _ => panic!("expected info"),
        }
        match &events[1] {
            DiagnosticEvent::Info { message, .. } => assert_eq!(message, "second"),
            _ => panic!("expected info"),
        }
        match &events[2] {
            DiagnosticEvent::Debug { message, .. } => assert_eq!(message, "third"),
            _ => panic!("expected debug"),
        }
    }

    #[test]
    fn drain_events_clears_buffer() {
        init(VerbosityConfig::default());
        drain_events(); // Clear existing

        emit_info(InfoFlag::Copy, 1, "test".to_string());
        let first_drain = drain_events();
        assert_eq!(first_drain.len(), 1);

        let second_drain = drain_events();
        assert_eq!(second_drain.len(), 0);
    }

    #[test]
    fn reinit_overwrites_config() {
        let mut config1 = VerbosityConfig::default();
        config1.info.copy = 5;
        init(config1);
        assert!(info_gte(InfoFlag::Copy, 5));

        let config2 = VerbosityConfig::default();
        init(config2);
        assert!(!info_gte(InfoFlag::Copy, 1));
    }
}
