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

pub trait RawLock:
    'static + Debug + Sized + Send + Sync + for<'s> HasGuard<'s, Lock = Self>
{
    fn new() -> Self;

    fn lock<'s>(&'s self) -> Result<Guard<'s, Self>, LockError>;

    fn lock_mut<'s>(&'s mut self) -> Guard<'s, Self>;

    fn park<'s>(
        &self,
        guard: Guard<'s, Self>,
        timeout: Option<Instant>,
    ) -> Result<(Guard<'s, Self>, bool), LockError>;

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

// This can be removed when GATs are available
pub trait HasGuard<'s> {
    type Lock;
    type Guard: Debug;
}

#[derive(Debug)]
#[repr(transparent)]
pub struct Guard<'s, L: HasGuard<'s, Lock = L>>(L::Guard);

impl<'s, L: HasGuard<'s, Lock = L>> Guard<'s, L> {
    #[inline]
    pub(crate) fn new(inner: L::Guard) -> Self {
        Self(inner)
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
            fn lock_contend() {
                let lock = $cls::new();
                let _guard = lock.lock().expect("Error locking");
                lock.lock().expect_err("Lock should fail");
            }

            #[test]
            fn lock_contend_threaded() {
                let lock = Arc::new($cls::new());
                let lock_copy = Arc::clone(&lock);
                let _guard = lock.lock().expect("Error locking");
                thread::spawn(move || {
                    lock_copy.lock().expect_err("Lock should fail");
                })
                .join()
                .unwrap();
            }

            #[test]
            fn park_notify() {
                let lock = $cls::new();
                for _ in 0..5 {
                    let guard = lock.lock().expect("Error locking");
                    assert_eq!(lock.notify(), true, "Expected notify to succeed");
                    let (_, notified) = lock.park(guard, None).expect("Error parking");
                    assert_eq!(notified, true, "Expected notified before park");
                }
            }

            #[test]
            fn park_notify_delay() {
                let lock = Arc::new($cls::new());
                for _ in 0..5 {
                    let lock_copy = Arc::clone(&lock);
                    let guard = lock.lock().expect("Error locking");
                    let th = thread::spawn(move || {
                        lock_copy.notify();
                    });
                    let expiry = Duration::from_millis(200).into_expire();
                    let (_guard, notified) = lock.park(guard, expiry).unwrap();
                    assert_eq!(notified, true, "Expected notified during park");
                    th.join().unwrap();
                }
            }

            #[test]
            fn park_timeout() {
                let lock = $cls::new();
                for _ in 0..5 {
                    let guard = lock.lock().expect("Error locking parker");
                    // no notifier
                    let expiry = Duration::from_millis(50).into_expire();
                    let (_guard, notified) = lock.park(guard, expiry).unwrap();
                    assert_eq!(notified, false, "Expected no notification");
                }
            }
        }
    };
}

parker_tests!(default_lock, DefaultLock);
parker_tests!(native_lock, NativeLock);
