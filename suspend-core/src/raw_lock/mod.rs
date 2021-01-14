use core::fmt::Debug;
use std::time::Instant;

use crate::error::LockError;

#[cfg(test)]
pub use self::default::{LockImpl as DefaultLock, ParkImpl as DefaultParker};

mod default;

cfg_if::cfg_if! {
    if #[cfg(any(target_os = "linux", target_os = "android"))] {
        mod futex;
        mod pthread;
        pub use self::pthread::LockImpl as NativeLock;
        pub use self::futex::ParkImpl as NativeParker;
    } else if #[cfg(unix)] {
        mod pthread;
        pub use self::pthread::LockImpl as NativeLock;
        pub use self::pthread::ParkImpl as NativeParker;
    } else {
        pub use self::default::LockImpl as NativeLock;
        pub use self::default::ParkImpl as NativeParker;
    }
}

pub trait RawInit: 'static + Debug + Sized + Send + Sync {
    fn new() -> Self;
}

pub trait RawParker: RawInit {
    fn acquire(&self) -> Result<(), LockError>;

    fn release(&self) -> Result<(), LockError>;

    fn park(&self, timeout: Option<Instant>) -> Result<Option<bool>, LockError>;

    fn unpark(&self) -> bool;
}

pub trait RawLock: RawInit + for<'g> HasGuard<'g> {
    fn lock(&self) -> Result<<Self as HasGuard<'_>>::Guard, LockError>;

    fn try_lock(&self) -> Result<<Self as HasGuard<'_>>::Guard, LockError>;

    fn notify_one(&self);

    fn notify_all(&self);
}

// This should not be needed once GATs are available
pub trait HasGuard<'g> {
    type Guard: RawGuard<'g>;
}

pub trait RawGuard<'g>: Debug + Sized {
    fn wait(self, timeout: Option<Instant>) -> Result<(Self, bool), LockError>;
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
            fn park_unpark() {
                let lock = $cls::new();
                for _ in 0..5 {
                    lock.acquire().expect("Error locking");
                    assert_eq!(lock.unpark(), true, "Expected notify to succeed");
                    let notified = lock.park(None).expect("Error parking");
                    assert_eq!(notified, Some(false), "Expected notified before park");
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
                        assert_eq!(lock_copy.unpark(), true, "Expected notify to succeed");
                    });
                    let expiry = Duration::from_millis(200).into_opt_instant();
                    let notified = lock.park(expiry).unwrap();
                    assert_eq!(notified.is_some(), true, "Expected notified during park");
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
                    let expiry = Duration::from_millis(50).into_opt_instant();
                    let notified = lock.park(expiry).unwrap();
                    assert_eq!(notified, None, "Expected no notification");
                    lock.release().expect("Error unlocking");
                }
            }
        }
    };
}

parker_tests!(default_park, DefaultParker);
parker_tests!(native_park, NativeParker);
