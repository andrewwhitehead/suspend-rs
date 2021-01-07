// Implementation borrowed from parking_lot_core:
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

use super::{LockError, RawInit, RawParker};

const STATE_FREE: i32 = 0b0000;
const STATE_HELD: i32 = 0b0010;
const STATE_PARK: i32 = 0b0100;
const STATE_UNPARK: i32 = 0b1000;

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
pub struct ParkImpl(AtomicI32);

impl RawInit for ParkImpl {
    #[inline]
    fn new() -> Self {
        Self(AtomicI32::new(STATE_FREE))
    }
}

impl RawParker for ParkImpl {
    #[inline]
    fn acquire(&self) -> Result<(), LockError> {
        if self.0.fetch_or(STATE_HELD, Ordering::Acquire) & STATE_HELD == 0 {
            Ok(())
        } else {
            Err(LockError::Contended)
        }
    }

    #[inline]
    fn release(&self) -> Result<(), LockError> {
        let found = self.0.fetch_and(!STATE_HELD, Ordering::Release);
        debug_assert!(found & STATE_HELD != 0);
        Ok(())
    }

    fn park(&self, timeout: Option<Instant>) -> Result<bool, LockError> {
        let futex = &self.0;

        // relaxed ordering is suffient, we will check again after acquiring the mutex
        let state = futex.load(Ordering::Relaxed);
        if state & STATE_UNPARK != 0 {
            // consume notification
            let found = futex.swap(state & !STATE_UNPARK, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok(true);
        }

        let state = match futex.compare_exchange(
            state,
            state | STATE_PARK,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(_) => state | STATE_PARK,
            Err(state) if state & STATE_UNPARK != 0 => {
                let found = futex.swap(state & !STATE_UNPARK, Ordering::Acquire);
                debug_assert_eq!(found, state);
                return Ok(true);
            }
            _ => panic!("Invalid lock state update"),
        };

        loop {
            let timed_out = futex_wait(futex, state, timeout)?;
            let state = futex.load(Ordering::Acquire);
            if timed_out || state & STATE_UNPARK != 0 {
                let found = futex.swap(state & !(STATE_PARK | STATE_UNPARK), Ordering::Acquire);
                return Ok(found & STATE_UNPARK != 0);
            }
        }
    }

    fn unpark(&self) -> bool {
        let futex = &self.0;
        let found = futex.fetch_or(STATE_UNPARK, Ordering::Release);
        if found & STATE_UNPARK == 0 {
            if found & STATE_PARK != 0 {
                futex_wake(futex);
            }
            true
        } else {
            false
        }
    }
}

impl Debug for ParkImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("futex::ParkImpl").finish()
    }
}

#[inline]
fn futex_wait(lock: &AtomicI32, state: i32, timeout: Option<Instant>) -> Result<bool, LockError> {
    let ts = if let Some(exp) = timeout {
        if let Some(diff) = exp.checked_duration_since(Instant::now()) {
            if diff.as_secs() as libc::time_t as u64 != diff.as_secs() {
                // Timeout overflowed, just sleep indefinitely
                None
            } else {
                Some(libc::timespec {
                    tv_sec: diff.as_secs() as libc::time_t,
                    tv_nsec: diff.subsec_nanos() as tv_nsec_t,
                })
            }
        } else {
            // Instant in the past
            return Ok(true);
        }
    } else {
        None
    };

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
    Err(LockError::InvalidState)
}

#[inline]
fn futex_wake(lock: &AtomicI32) -> bool {
    let r = unsafe {
        libc::syscall(
            libc::SYS_futex,
            lock as *const AtomicI32,
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            1i32,
        )
    };
    debug_assert!(r == 0 || r == 1 || r == -1);
    if r == -1 {
        // the parked thread has likely been freed
        debug_assert_eq!(errno(), libc::EFAULT);
    }
    r == 1
}
