use core::fmt::Debug;

use crate::error::LockError;
use crate::types::{Expiry, ParkResult};

#[cfg(all(test, feature = "std"))]
pub use self::default::ParkImpl as DefaultParker;

#[cfg(feature = "std")]
mod default;

cfg_if::cfg_if! {
    if #[cfg(any(target_os = "linux", target_os = "android"))] {
        mod futex;
        mod pthread;
        pub use self::futex::ParkImpl as NativeParker;
    } else if #[cfg(unix)] {
        mod pthread;
        pub use self::pthread::ParkImpl as NativeParker;
    } else {
        pub use self::default::ParkImpl as NativeParker;
    }
}

pub trait RawParker: Debug {
    fn acquire(&self) -> Result<(), LockError>;

    fn release(&self) -> Result<(), LockError>;

    fn park(&self, timeout: Expiry) -> Result<ParkResult, LockError>;

    fn unpark(&self) -> bool;
}

macro_rules! parker_tests {
    ($mod:ident, $cls:ident) => {
        #[cfg(all(test, feature = "std"))]
        mod $mod {
            use super::*;
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
                    let notified = lock.park(None.into()).expect("Error parking");
                    assert_eq!(notified.skipped(), true, "Expected notified before park");
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
                    let expiry = Duration::from_millis(200).into();
                    let notified = lock.park(expiry).unwrap();
                    assert_eq!(notified.timed_out(), false, "Expected notified during park");
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
                    let expiry = Duration::from_millis(50).into();
                    let notified = lock.park(expiry).unwrap();
                    assert_eq!(notified.timed_out(), true, "Expected no notification");
                    lock.release().expect("Error unlocking");
                }
            }
        }
    };
}

parker_tests!(default_park, DefaultParker);
parker_tests!(native_park, NativeParker);
