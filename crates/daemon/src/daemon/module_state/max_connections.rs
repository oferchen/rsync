use std::num::NonZeroU32;

/// A module's `max connections` setting.
///
/// upstream: the directive is a P_INTEGER read with `atoi()`
/// (loadparm.c:431-433), and connection.c:claim_connection:26-46 gives the
/// resulting integer three distinct meanings: zero is unlimited and takes no
/// lock, a positive value is a slot count, and a negative value can never
/// satisfy `for (i = 0; i < max_connections; i++)` so every attempt is
/// refused - documented in rsyncd.conf.5 as "A negative value disables the
/// module".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MaxConnections {
    /// `0` - no limit; upstream returns success without taking a lock.
    #[default]
    Unlimited,
    /// A positive slot count.
    Limited(NonZeroU32),
    /// A negative value; the module refuses every connection. Carries the
    /// configured number so the diagnostic can echo it verbatim, matching
    /// upstream's `@ERROR: max connections (%d) reached -- try again later`.
    Disabled(i32),
}

impl MaxConnections {
    /// Classifies the integer stored for a `max connections` directive.
    ///
    /// upstream: connection.c:claim_connection:26-46 - `0` returns before the
    /// lock file is opened, a positive count bounds the slot scan, and a
    /// negative count leaves the scan empty so the module is disabled.
    pub(crate) const fn from_configured(value: i32) -> Self {
        if value < 0 {
            return Self::Disabled(value);
        }

        // `value` is non-negative here, so the cast is exact and
        // `NonZeroU32::new` yields `None` only for the unlimited case.
        match NonZeroU32::new(value as u32) {
            Some(limit) => Self::Limited(limit),
            None => Self::Unlimited,
        }
    }

    /// Returns the slot-scan bound, which is zero unless a positive count was
    /// configured.
    ///
    /// upstream: connection.c:33 `for (i = 0; i < max_connections; i++)` uses
    /// the raw integer as the loop bound, so a negative value runs no
    /// iterations and claims no slot.
    pub(crate) const fn slot_count(self) -> u32 {
        match self {
            Self::Limited(limit) => limit.get(),
            Self::Unlimited | Self::Disabled(_) => 0,
        }
    }

    /// Returns the number echoed in the refusal diagnostic, preserving the
    /// minus sign of a disabling value.
    ///
    /// upstream: clientserver.c:746-757 formats the configured integer with
    /// `%d` into `@ERROR: max connections (%d) reached -- try again later`.
    pub(crate) const fn display_value(self) -> i32 {
        match self {
            Self::Unlimited => 0,
            Self::Limited(limit) => limit.get() as i32,
            Self::Disabled(value) => value,
        }
    }
}
