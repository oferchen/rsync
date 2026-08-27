// Operator gating for the daemon's kernel-enforced sandbox layers.
//
// oc wraps a daemon worker in two layers upstream rsync does not have at
// all: the Landlock LSM path allowlist (`fast_io::landlock`) and the
// seccomp BPF syscall allowlist (`sections/seccomp.rs`). Neither ruleset
// can be derived from upstream, but upstream does state a policy for
// coexisting with an externally imposed syscall filter, and it is the
// policy this module implements: `generator.c:1444-1487` decides whether a
// confined primitive exists from the BUILD, never from a runtime errno,
// because "a live mknodat() or mkfifoat() can return ENOSYS too (an
// unimplemented FUSE mknod, or seccomp)". Whether a layer is installed is
// therefore always a declared decision - a compile-time feature plus an
// explicit operator opt-out - and never an inference from a syscall that
// happened to fail.
//
// One owner for both layers. A layer names two environment variables of
// opposite polarity; both are parsed by the shared predicates below, so
// adding a third layer is one enum arm and no new parsing.

/// A kernel-enforced sandbox layer the daemon installs around a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SandboxLayer {
    /// Landlock LSM path allowlist, engaged after chroot and privilege drop.
    Landlock,
    /// seccomp BPF syscall allowlist, engaged immediately after Landlock.
    Seccomp,
}

impl SandboxLayer {
    /// Lower-case layer name used in the `<layer>=skipped` operator log.
    const fn label(self) -> &'static str {
        match self {
            Self::Landlock => "landlock",
            Self::Seccomp => "seccomp",
        }
    }

    /// Variable whose truthy value disables the layer (`OC_RSYNC_NO_*`).
    const fn disable_var(self) -> &'static str {
        match self {
            Self::Landlock => "OC_RSYNC_NO_LANDLOCK",
            Self::Seccomp => "OC_RSYNC_NO_SECCOMP",
        }
    }

    /// Variable whose falsy value disables the layer (`OC_RSYNC_DAEMON_*`).
    ///
    /// Inverse polarity, kept because `OC_RSYNC_DAEMON_SECCOMP` was the
    /// historical opt-in spelling: an operator who set it to `1` can flip
    /// the same name to `0` rather than learn a second one.
    const fn enable_var(self) -> &'static str {
        match self {
            Self::Landlock => "OC_RSYNC_DAEMON_LANDLOCK",
            Self::Seccomp => "OC_RSYNC_DAEMON_SECCOMP",
        }
    }

    /// Name of the variable that opted this layer out, or `None` to engage.
    ///
    /// The variable name is returned rather than a rendered sentence so the
    /// caller can state exactly which knob the operator set - a skip that
    /// does not name its own cause is indistinguishable from a layer that
    /// was never available.
    fn operator_optout_var(self) -> Option<&'static str> {
        if env_flag_truthy(self.disable_var()) {
            return Some(self.disable_var());
        }
        if env_flag_negated(self.enable_var()) {
            return Some(self.enable_var());
        }
        None
    }

    /// Renders the operator-facing skip line for `module`'s request.
    fn optout_log_text(self, request: &str, var: &str) -> String {
        format!(
            "module '{request}': {}=skipped reason={var} set by operator (this layer is NOT installed)",
            self.label(),
        )
    }
}

/// True when `var` is set to anything other than empty, `0`, or `false`.
fn env_flag_truthy(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

/// True when `var` is set to `0` or `false`.
fn env_flag_negated(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("false"))
}

#[cfg(test)]
mod sandbox_optout_tests {
    use super::*;
    use crate::test_env::{ENV_LOCK, EnvGuard};
    use std::ffi::OsStr;

    const LAYERS: [SandboxLayer; 2] = [SandboxLayer::Landlock, SandboxLayer::Seccomp];

    #[test]
    fn every_layer_engages_when_no_variable_is_set() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for layer in LAYERS {
            let _disable = EnvGuard::remove(layer.disable_var());
            let _enable = EnvGuard::remove(layer.enable_var());
            assert_eq!(layer.operator_optout_var(), None, "{layer:?}");
        }
    }

    #[test]
    fn a_truthy_disable_variable_opts_the_layer_out() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for layer in LAYERS {
            let _enable = EnvGuard::remove(layer.enable_var());
            for value in ["1", "yes", "true", "on"] {
                let _disable = EnvGuard::set(layer.disable_var(), OsStr::new(value));
                assert_eq!(
                    layer.operator_optout_var(),
                    Some(layer.disable_var()),
                    "{layer:?} value {value}",
                );
            }
        }
    }

    #[test]
    fn an_empty_or_false_disable_variable_leaves_the_layer_engaged() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for layer in LAYERS {
            let _enable = EnvGuard::remove(layer.enable_var());
            for value in ["", "0", "false", "FALSE"] {
                let _disable = EnvGuard::set(layer.disable_var(), OsStr::new(value));
                assert_eq!(layer.operator_optout_var(), None, "{layer:?} value {value}",);
            }
        }
    }

    #[test]
    fn a_negated_enable_variable_opts_the_layer_out() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for layer in LAYERS {
            let _disable = EnvGuard::remove(layer.disable_var());
            for value in ["0", "false", "False"] {
                let _enable = EnvGuard::set(layer.enable_var(), OsStr::new(value));
                assert_eq!(
                    layer.operator_optout_var(),
                    Some(layer.enable_var()),
                    "{layer:?} value {value}",
                );
            }
        }
    }

    #[test]
    fn a_truthy_enable_variable_leaves_the_layer_engaged() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for layer in LAYERS {
            let _disable = EnvGuard::remove(layer.disable_var());
            let _enable = EnvGuard::set(layer.enable_var(), OsStr::new("1"));
            assert_eq!(layer.operator_optout_var(), None, "{layer:?}");
        }
    }

    #[test]
    fn the_two_layers_read_disjoint_variables() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for layer in LAYERS {
            let other = if layer == SandboxLayer::Landlock {
                SandboxLayer::Seccomp
            } else {
                SandboxLayer::Landlock
            };
            let _a = EnvGuard::remove(layer.disable_var());
            let _b = EnvGuard::remove(layer.enable_var());
            let _c = EnvGuard::remove(other.enable_var());
            let _d = EnvGuard::set(other.disable_var(), OsStr::new("1"));
            assert_eq!(
                layer.operator_optout_var(),
                None,
                "{layer:?} must not read {}",
                other.disable_var(),
            );
        }
    }

    #[test]
    fn the_skip_line_names_the_layer_and_the_variable() {
        let text = SandboxLayer::Landlock.optout_log_text("data", "OC_RSYNC_NO_LANDLOCK");
        assert!(text.contains("module 'data'"), "{text}");
        assert!(text.contains("landlock=skipped"), "{text}");
        assert!(text.contains("reason=OC_RSYNC_NO_LANDLOCK"), "{text}");
    }
}
