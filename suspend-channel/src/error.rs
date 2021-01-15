use core::fmt::{self, Display, Formatter};

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

#[cfg(feature = "std")]
impl ::std::error::Error for RecvError {}

/// An error indicating that a `try_send` operation could not be completed
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum TrySendError<T> {
    /// The receiver has been dropped
    Disconnected(T),
    /// The channel is full
    Full(T),
}

impl<T> TrySendError<T> {
    /// Determine whether the send failed because the receiver was dropped
    pub fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected(..))
    }

    /// Determine whether the send failed because the channel is already full
    pub fn is_full(&self) -> bool {
        matches!(self, Self::Full(..))
    }

    /// Unwrap the inner value from the error
    pub fn into_inner(self) -> T {
        match self {
            Self::Disconnected(val) | Self::Full(val) => val,
        }
    }
}
