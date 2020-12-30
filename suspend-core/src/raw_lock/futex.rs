// Copyright 2016 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use core::{
    fmt::{self, Debug, Formatter},
    ptr,
    sync::atomic::{AtomicI32, Ordering},
};
use libc;
use std::time::Instant;

use super::{Guard, HasGuard, LockError, RawLock};

const STATE_FREE: i32 = 0b0000;
const STATE_HELD: i32 = 0b0001;
const STATE_PARK: i32 = 0b0010;
const STATE_NOTIFY: i32 = 0b0100;

// x32 Linux uses a non-standard type for tv_nsec in timespec.
// See https://sourceware.org/bugzilla/show_bug.cgi?id=16437
#[cfg(all(target_arch = "x86_64", target_pointer_width = "32"))]
#[allow(non_camel_case_types)]
type tv_nsec_t = i64;
#[cfg(not(all(target_arch = "x86_64", target_pointer_width = "32")))]
#[allow(non_camel_case_types)]
type tv_nsec_t = libc::c_long;

fn errno() -> libc::c_int {
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location()
    }
    #[cfg(not(target_os = "linux"))]
    unsafe {
        *libc::__errno()
    }
}

// align 4?
#[repr(transparent)]
pub struct LockImpl(AtomicI32);

impl RawLock for LockImpl {
    #[inline]
    fn new() -> Self {
        Self(AtomicI32::new(STATE_FREE))
    }

    #[inline]
    fn lock<'s>(&'s self) -> Result<Guard<'s, Self>, LockError> {
        if self.0.fetch_or(STATE_HELD, Ordering::Acquire) & STATE_HELD == 0 {
            Ok(Guard::new(GuardImpl(self, true)))
        } else {
            Err(LockError::Contended)
        }
    }

    #[inline]
    fn lock_mut<'s>(&'s mut self) -> Guard<'s, Self> {
        Guard(GuardImpl(self, false))
    }

    fn park<'s>(
        &self,
        guard: Guard<'s, Self>,
        timeout: Option<Instant>,
    ) -> Result<(Guard<'s, Self>, bool), LockError> {
        if !ptr::eq(self, (guard.0).0) {
            return Err(LockError::Invalid);
        }
        let futex = &self.0;

        // relaxed ordering is suffient, we will check again after acquiring the mutex
        let state = futex.load(Ordering::Relaxed);
        if state & STATE_NOTIFY != 0 {
            // consume notification
            let found = futex.swap(state & !STATE_NOTIFY, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok((guard, true));
        }

        let state = match futex.compare_exchange(
            state,
            state | STATE_PARK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => state | STATE_PARK,
            Err(state) if state & STATE_NOTIFY != 0 => {
                let found = futex.swap(state & !STATE_NOTIFY, Ordering::Acquire);
                debug_assert_eq!(found, state);
                return Ok((guard, true));
            }
            _ => panic!("Invalid lock state update"),
        };

        loop {
            let (ts, mut timed_out) = if let Some(exp) = timeout {
                if let Some(diff) = exp.checked_duration_since(Instant::now()) {
                    if diff.as_secs() as libc::time_t as u64 != diff.as_secs() {
                        // Timeout overflowed, just sleep indefinitely
                        (None, false)
                    } else {
                        (
                            Some(libc::timespec {
                                tv_sec: diff.as_secs() as libc::time_t,
                                tv_nsec: diff.subsec_nanos() as tv_nsec_t,
                            }),
                            false,
                        )
                    }
                } else {
                    (None, true)
                }
            } else {
                (None, false)
            };

            if !timed_out {
                timed_out = futex_wait(futex, state, ts)?;
            }

            let state = futex.load(Ordering::Relaxed);
            if timed_out || state & STATE_NOTIFY != 0 {
                let found = futex.swap(state & !(STATE_PARK | STATE_NOTIFY), Ordering::Acquire);
                return Ok((guard, found & STATE_NOTIFY != 0));
            }
        }
    }

    fn notify(&self) -> bool {
        let futex = &self.0;
        let found = futex.fetch_or(STATE_NOTIFY, Ordering::Release);
        if found & STATE_NOTIFY == 0 {
            if found & STATE_PARK != 0 {
                let r = unsafe {
                    libc::syscall(
                        libc::SYS_futex,
                        futex as *const AtomicI32,
                        libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                        1i32,
                    )
                };
                debug_assert!(r == 0 || r == 1 || r == -1);
                if r == -1 {
                    // the parked thread has been freed
                    debug_assert_eq!(errno(), libc::EFAULT);
                }
            }
            true
        } else {
            false
        }
    }
}

impl Debug for LockImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("futex::LockImpl").finish()
    }
}

impl<'s> HasGuard<'s> for LockImpl {
    type Lock = LockImpl;
    type Guard = GuardImpl<'s>;
}

#[derive(Debug)]
pub struct GuardImpl<'s>(&'s LockImpl, bool);

impl<'s> Drop for GuardImpl<'s> {
    fn drop(&mut self) {
        if self.1 {
            (self.0).0.fetch_and(!STATE_HELD, Ordering::Release);
        }
    }
}

#[inline]
fn futex_wait(lock: &AtomicI32, state: i32, ts: Option<libc::timespec>) -> Result<bool, LockError> {
    let ts_ptr = ts
        .as_ref()
        .map(|ts_ref| ts_ref as *const _)
        .unwrap_or(ptr::null());
    let r = unsafe {
        libc::syscall(
            libc::SYS_futex,
            lock,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            state,
            ts_ptr,
        )
    };
    if r == 0 {
        return Ok(false);
    } else if r == -1 {
        let err = errno();
        if ts.is_some() && err == libc::ETIMEDOUT {
            return Ok(true);
        }
        if err == libc::EINTR || err == libc::EAGAIN {
            return Ok(false);
        }
    }
    Err(LockError::Invalid)
}
