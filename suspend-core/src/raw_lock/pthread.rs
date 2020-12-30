// Borrowed from parking_lot_core
// Copyright 2016 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use core::{
    cell::UnsafeCell,
    fmt::{self, Debug, Formatter},
    mem::MaybeUninit,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use libc;
use std::time::Instant;

use super::{Guard, HasGuard, LockError, RawLock};

// x32 Linux uses a non-standard type for tv_nsec in timespec.
// See https://sourceware.org/bugzilla/show_bug.cgi?id=16437
#[cfg(all(target_arch = "x86_64", target_pointer_width = "32"))]
#[allow(non_camel_case_types)]
type tv_nsec_t = i64;
#[cfg(not(all(target_arch = "x86_64", target_pointer_width = "32")))]
#[allow(non_camel_case_types)]
type tv_nsec_t = libc::c_long;

const STATE_FREE: usize = 0b0000;
const STATE_HELD: usize = 0b0001;
const STATE_PARK: usize = 0b0010;
const STATE_NOTIFY: usize = 0b0100;

pub struct LockImpl {
    cond: UnsafeCell<libc::pthread_cond_t>,
    mutex: UnsafeCell<libc::pthread_mutex_t>,
    state: AtomicUsize,
}

impl RawLock for LockImpl {
    #[inline]
    fn new() -> Self {
        let slf = Self {
            cond: UnsafeCell::new(libc::PTHREAD_COND_INITIALIZER),
            mutex: UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER),
            state: AtomicUsize::new(STATE_FREE),
        };
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
        condvar_init(slf.cond.get());
        slf
    }

    #[inline]
    fn lock<'s>(&'s self) -> Result<Guard<'s, Self>, LockError> {
        if self.state.fetch_or(STATE_HELD, Ordering::Acquire) & STATE_HELD == 0 {
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

        // relaxed ordering is suffient, we will check again after acquiring the mutex
        let state = self.state.load(Ordering::Relaxed);
        if state & STATE_NOTIFY != 0 {
            // consume notification
            let found = self.state.swap(state & !STATE_NOTIFY, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok((guard, true));
        }

        let r = unsafe { libc::pthread_mutex_lock(self.mutex.get()) };
        if r != 0 {
            return Err(LockError::Invalid);
        }

        match self.state.compare_exchange(
            state,
            state | STATE_PARK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => (),
            Err(state) if state & STATE_NOTIFY != 0 => {
                let r = unsafe { libc::pthread_mutex_unlock(self.mutex.get()) };
                assert_eq!(r, 0);
                let found = self.state.swap(state & !STATE_NOTIFY, Ordering::Acquire);
                debug_assert_eq!(found, state);
                return Ok((guard, true));
            }
            _ => panic!("Invalid lock state update"),
        }

        loop {
            let timeout_state = if let Some(exp) = timeout {
                if let Some(dur) = exp.checked_duration_since(Instant::now()) {
                    if let Some(ts) = timeout_to_timespec(dur) {
                        let r = unsafe {
                            libc::pthread_cond_timedwait(self.cond.get(), self.mutex.get(), &ts)
                        };
                        if r == libc::ETIMEDOUT {
                            Some(true)
                        } else if r == 0 || (ts.tv_sec < 0 && r == libc::EINVAL) {
                            // some systems return EINVAL for a negative timeout. in
                            // those cases we just loop continuously
                            Some(false)
                        } else {
                            return Err(LockError::Invalid);
                        }
                    } else {
                        None // overflowed, just park indefinitely
                    }
                } else {
                    Some(true)
                }
            } else {
                None
            };

            let timed_out = if let Some(t) = timeout_state {
                t
            } else {
                let r = unsafe { libc::pthread_cond_wait(self.cond.get(), self.mutex.get()) };
                if r != 0 {
                    return Err(LockError::Invalid);
                }
                false
            };

            let state = self.state.load(Ordering::Relaxed);
            if timed_out || state & STATE_NOTIFY != 0 {
                let found = self
                    .state
                    .swap(state & !(STATE_PARK | STATE_NOTIFY), Ordering::Acquire);
                let r = unsafe { libc::pthread_mutex_unlock(self.mutex.get()) };
                assert_eq!(r, 0);
                return Ok((guard, found & STATE_NOTIFY != 0));
            }
        }
    }

    fn notify(&self) -> bool {
        let found = self.state.fetch_or(STATE_NOTIFY, Ordering::Release);
        if found & STATE_NOTIFY == 0 {
            if found & STATE_PARK != 0 {
                unsafe {
                    // acquire the mutex because the parking thread could be interrupted
                    // between setting the state and waiting on the condvar
                    let r = libc::pthread_mutex_lock(self.mutex.get());
                    assert_eq!(r, 0);
                    let r = libc::pthread_mutex_unlock(self.mutex.get());
                    assert_eq!(r, 0);
                    // wake the parked thread
                    let r = libc::pthread_cond_signal(self.cond.get());
                    assert_eq!(r, 0);
                }
            }
            true
        } else {
            false
        }
    }
}

impl Drop for LockImpl {
    #[inline]
    fn drop(&mut self) {
        // On DragonFly pthread_mutex_destroy() returns EINVAL if called on a
        // mutex that was just initialized with libc::PTHREAD_MUTEX_INITIALIZER.
        // Once it is used (locked/unlocked) or pthread_mutex_init() is called,
        // this behaviour no longer occurs. The same applies to condvars.
        unsafe {
            let r = libc::pthread_mutex_destroy(self.mutex.get());
            debug_assert!(r == 0 || r == libc::EINVAL);
            let r = libc::pthread_cond_destroy(self.cond.get());
            debug_assert!(r == 0 || r == libc::EINVAL);
        }
    }
}

unsafe impl Send for LockImpl {}
unsafe impl Sync for LockImpl {}

impl Debug for LockImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("pthread::LockImpl").finish()
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
            self.0.state.fetch_and(!STATE_HELD, Ordering::Release);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
#[inline]
unsafe fn condvar_init(cvar: *mut libc::pthread_cond_t) {
    // Initializes the condvar to use CLOCK_MONOTONIC instead of CLOCK_REALTIME.
    let mut attr = MaybeUninit::<libc::pthread_condattr_t>::uninit();
    let r = libc::pthread_condattr_init(attr.as_mut_ptr());
    debug_assert_eq!(r, 0);
    let r = libc::pthread_condattr_setclock(attr.as_mut_ptr(), libc::CLOCK_MONOTONIC);
    debug_assert_eq!(r, 0);
    let r = libc::pthread_cond_init(cvar, attr.as_ptr());
    debug_assert_eq!(r, 0);
    let r = libc::pthread_condattr_destroy(attr.as_mut_ptr());
    debug_assert_eq!(r, 0);
}

// Returns the current time on the clock used by pthread_cond_t as a timespec.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[inline]
fn timespec_now() -> libc::timespec {
    let mut now = MaybeUninit::<libc::timeval>::uninit();
    let r = unsafe { libc::gettimeofday(now.as_mut_ptr(), ptr::null_mut()) };
    debug_assert_eq!(r, 0);
    // SAFETY: We know `libc::gettimeofday` has initialized the value.
    let now = unsafe { now.assume_init() };
    libc::timespec {
        tv_sec: now.tv_sec,
        tv_nsec: now.tv_usec as tv_nsec_t * 1000,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
#[inline]
fn timespec_now() -> libc::timespec {
    let mut now = MaybeUninit::<libc::timespec>::uninit();
    let clock = if cfg!(target_os = "android") {
        // Android doesn't support pthread_condattr_setclock, so we need to
        // specify the timeout in CLOCK_REALTIME.
        libc::CLOCK_REALTIME
    } else {
        libc::CLOCK_MONOTONIC
    };
    let r = unsafe { libc::clock_gettime(clock, now.as_mut_ptr()) };
    debug_assert_eq!(r, 0);
    // SAFETY: We know `libc::clock_gettime` has initialized the value.
    unsafe { now.assume_init() }
}

// Converts a relative timeout into an absolute timeout in the clock used by
// pthread_cond_t.
#[inline]
fn timeout_to_timespec(timeout: Duration) -> Option<libc::timespec> {
    // Handle overflows early on
    if timeout.as_secs() > libc::time_t::max_value() as u64 {
        return None;
    }

    let now = timespec_now();
    let mut nsec = now.tv_nsec + timeout.subsec_nanos() as tv_nsec_t;
    let mut sec = now.tv_sec.checked_add(timeout.as_secs() as libc::time_t);
    if nsec >= 1_000_000_000 {
        nsec -= 1_000_000_000;
        sec = sec.and_then(|sec| sec.checked_add(1));
    }

    sec.map(|sec| libc::timespec {
        tv_nsec: nsec,
        tv_sec: sec,
    })
}
