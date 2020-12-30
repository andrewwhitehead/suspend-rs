use core::time::Duration;
use std::time::Instant;

/// A trait for supporting multiple expiry time input formats.
pub trait Expiry {
    /// Convert the input value to an `Option<Instant>`.
    fn into_expire(self) -> Option<Instant>;
}

impl Expiry for Duration {
    #[inline]
    fn into_expire(self) -> Option<Instant> {
        Some(Instant::now() + self)
    }
}

impl Expiry for Instant {
    #[inline]
    fn into_expire(self) -> Option<Instant> {
        Some(self)
    }
}

impl Expiry for Option<Instant> {
    #[inline]
    fn into_expire(self) -> Option<Instant> {
        self
    }
}
