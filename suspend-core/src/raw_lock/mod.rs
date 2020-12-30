use core::fmt::{self, Debug, Display, Formatter};
use std::time::Instant;

#[cfg(test)]
pub use self::default::LockImpl as DefaultLock;
pub use self::imp::LockImpl as NativeLock;

#[cfg(test)]
mod default;

cfg_if::cfg_if! {
    if #[cfg(any(target_os = "linux", target_os = "android"))] {
        #[path = "futex.rs"]
        mod imp;
    } else if #[cfg(unix)] {
        #[path = "pthread.rs"]
        mod imp;
    } else {
        #[path = "default.rs"]
        mod imp;
    }
}

pub trait RawLock: 'static + Debug + Sized + Send + Sync {
    fn new() -> Self;

    fn acquire(&self) -> Result<(), LockError>;

    fn release(&self) -> Result<(), LockError>;

    fn park(&self, timeout: Option<Instant>) -> Result<bool, LockError>;

    fn notify(&self) -> bool;
}

#[derive(Debug)]
pub enum LockError {
    Cancelled,
    Contended,
    Invalid,
    Poisoned,
}

impl Display for LockError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("LockError")
    }
}

macro_rules! parker_tests {
    ($mod:ident, $cls:ident) => {
        #[cfg(test)]
        mod $mod {
            use super::*;
            use crate::types::Expiry;
            use std::sync::Arc;
            use std::thread;
            use std::time::Duration;

            #[test]
            fn acquire_contend() {
                let lock = $cls::new();
                lock.acquire().expect("Error locking");
                lock.acquire().expect_err("Lock should fail");
            }

            #[test]
            fn acquire_contend_threaded() {
                let lock = Arc::new($cls::new());
                let lock_copy = Arc::clone(&lock);
                lock.acquire().expect("Error locking");
                thread::spawn(move || {
                    lock_copy.acquire().expect_err("Lock should fail");
                })
                .join()
                .unwrap();
            }

            #[test]
            fn park_notify() {
                let lock = $cls::new();
                for _ in 0..5 {
                    lock.acquire().expect("Error locking");
                    assert_eq!(lock.notify(), true, "Expected notify to succeed");
                    let notified = lock.park(None).expect("Error parking");
                    assert_eq!(notified, true, "Expected notified before park");
                    lock.release().expect("Error unlocking");
                }
            }

            #[test]
            fn park_notify_delay() {
                let lock = Arc::new($cls::new());
                for _ in 0..5 {
                    let lock_copy = Arc::clone(&lock);
                    lock.acquire().expect("Error locking");
                    let th = thread::spawn(move || {
                        lock_copy.notify();
                    });
                    let expiry = Duration::from_millis(200).into_expire();
                    let notified = lock.park(expiry).unwrap();
                    assert_eq!(notified, true, "Expected notified during park");
                    th.join().unwrap();
                    lock.release().expect("Error unlocking");
                }
            }

            #[test]
            fn park_timeout() {
                let lock = $cls::new();
                for _ in 0..5 {
                    lock.acquire().expect("Error locking parker");
                    // no notifier
                    let expiry = Duration::from_millis(50).into_expire();
                    let notified = lock.park(expiry).unwrap();
                    assert_eq!(notified, false, "Expected no notification");
                    lock.release().expect("Error unlocking");
                }
            }
        }
    };
}

parker_tests!(default_lock, DefaultLock);
parker_tests!(native_lock, NativeLock);
