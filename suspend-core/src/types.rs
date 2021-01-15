use core::time::Duration;

#[cfg(feature = "std")]
use std::time::Instant;

/// A compatibility wrapper around an optional expiry time
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Expiry(
    #[cfg(feature = "std")] Option<Instant>,
    #[cfg(not(feature = "std"))] Option<Duration>,
);

impl Expiry {
    /// Determine if there is a defined expiry
    pub const fn is_some(self) -> bool {
        self.0.is_some()
    }

    /// Convert the expiry into a duration from now
    #[cfg(feature = "std")]
    #[inline]
    pub fn checked_duration(self) -> Option<Duration> {
        self.0
            .and_then(|inst| inst.checked_duration_since(Instant::now()))
    }

    /// Convert the expiry into a duration from now
    #[cfg(not(feature = "std"))]
    pub const fn checked_duration(self) -> Option<Duration> {
        self.0
    }

    /// Unwrap the expiry as an instant
    #[cfg(feature = "std")]
    pub(crate) const fn into_opt_instant(self) -> Option<Instant> {
        self.0
    }
}

#[cfg(feature = "std")]
impl From<Duration> for Expiry {
    #[inline]
    fn from(dur: Duration) -> Expiry {
        Self(Some(Instant::now() + dur))
    }
}

#[cfg(not(feature = "std"))]
impl From<Duration> for Expiry {
    #[inline]
    fn from(dur: Duration) -> Expiry {
        Self(Some(dur))
    }
}

#[cfg(feature = "std")]
impl From<Instant> for Expiry {
    #[inline]
    fn from(inst: Instant) -> Expiry {
        Self(Some(inst))
    }
}

#[cfg(feature = "std")]
impl From<Option<Duration>> for Expiry {
    #[inline]
    fn from(dur: Option<Duration>) -> Expiry {
        Self(dur.map(|d| Instant::now() + d))
    }
}

#[cfg(not(feature = "std"))]
impl From<Option<Duration>> for Expiry {
    #[inline]
    fn from(dur: Option<Duration>) -> Expiry {
        Self(dur)
    }
}

/// The result of a thread `park` operation
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ParkResult {
    /// The park was skipped because a pending notification was consumed
    Skipped,
    /// The park timed out
    TimedOut,
    /// The thread was parked and subsequently notified
    Unparked,
}

impl ParkResult {
    /// Determine if the park operation timed out
    pub const fn timed_out(self) -> bool {
        matches!(self, Self::TimedOut)
    }

    /// Determine if the park operation was skipped due to a pending notification
    pub const fn skipped(self) -> bool {
        matches!(self, Self::Skipped)
    }
}
