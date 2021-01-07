use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// An error indicating that a receive operation could not be completed
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum RecvError {
    /// The sender dropped before producing a message
    Incomplete,
    /// The receiver has already produced a result
    Terminated,
    /// No message was received in time
    TimedOut,
}

impl RecvError {
    /// A helper function to check for a timeout
    pub fn timed_out(&self) -> bool {
        matches!(self, Self::TimedOut)
    }
}

impl Display for RecvError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Incomplete => "Incomplete",
            Self::Terminated => "Terminated",
            Self::TimedOut => "Timed out",
        })
    }
}

impl Error for RecvError {}
