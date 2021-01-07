use core::fmt::{self, Display, Formatter};

/// Potential errors raised by lock operations
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LockError {
    /// The lock is already held
    Contended,
    /// The lock instance is invalid
    InvalidState,
    /// The lock was poisoned by another thread
    Poisoned,
}

impl Display for LockError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("LockError")
    }
}
