use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// An error indicating that a receive operation could not be completed because
/// the sender was dropped.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Incomplete;

impl Display for Incomplete {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Incomplete")
    }
}

impl Error for Incomplete {}

/// An error returned when a timeout has expired.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TimedOut;

impl Display for TimedOut {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Timed out")
    }
}

impl Error for TimedOut {}
