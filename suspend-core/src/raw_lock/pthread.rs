// Implementation borrowed from parking_lot_core:
// Copyright 2016 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use core::{
    cell::UnsafeCell,
    fmt::{self, Debug, Formatter},
    mem::{ManuallyDrop, MaybeUninit},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use libc;
use std::time::Instant;

use super::{HasGuard, LockError, RawGuard, RawInit, RawLock, RawParker};

const STATE_FREE: usize = 0b0000;
const STATE_INIT: usize = 0b0001;
const STATE_HELD: usize = 0b0010;
const STATE_PARK: usize = 0b0100;
const STATE_UNPARK: usize = 0b1000;

// x32 Linux uses a non-standard type for tv_nsec in timespec.
// See https://sourceware.org/bugzilla/show_bug.cgi?id=16437
#[cfg(all(target_arch = "x86_64", target_pointer_width = "32"))]
#[allow(non_camel_case_types)]
type tv_nsec_t = i64;
#[cfg(not(all(target_arch = "x86_64", target_pointer_width = "32")))]
#[allow(non_camel_case_types)]
type tv_nsec_t = libc::c_long;

pub struct LockImpl {
    cond: UnsafeCell<libc::pthread_cond_t>,
    mutex: UnsafeCell<libc::pthread_mutex_t>,
}

impl LockImpl {
    pub const fn new_uninit() -> Self {
        Self {
            cond: UnsafeCell::new(libc::PTHREAD_COND_INITIALIZER),
            mutex: UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER),
        }
    }

    #[inline]
    pub fn init(&self) {
        mutex_init(self.mutex.get());
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
        condvar_init(self.cond.get());
    }
}

impl RawInit for LockImpl {
    #[inline]
    fn new() -> Self {
        let slf = Self::new_uninit();
        slf.init();
        slf
    }
}

impl RawLock for LockImpl {
    #[inline]
    fn lock(&self) -> Result<GuardImpl, LockError> {
        let r = unsafe { libc::pthread_mutex_lock(self.mutex.get()) };
        if r != 0 {
            return Err(LockError::InvalidState);
        }
        Ok(GuardImpl {
            mutex: self.mutex.get(),
            cond: self.cond.get(),
        })
    }

    #[inline]
    fn try_lock(&self) -> Result<<Self as HasGuard<'_>>::Guard, LockError> {
        let r = unsafe { libc::pthread_mutex_trylock(self.mutex.get()) };
        if r == libc::EBUSY {
            return Err(LockError::Contended);
        } else if r != 0 {
            return Err(LockError::InvalidState);
        }
        Ok(GuardImpl {
            mutex: self.mutex.get(),
            cond: self.cond.get(),
        })
    }

    #[inline]
    fn notify_one(&self) {
        let r = unsafe { libc::pthread_cond_signal(self.cond.get()) };
        assert_eq!(r, 0);
    }

    #[inline]
    fn notify_all(&self) {
        let r = unsafe { libc::pthread_cond_broadcast(self.cond.get()) };
        assert_eq!(r, 0);
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

impl<'g> HasGuard<'g> for LockImpl {
    type Guard = GuardImpl;
}

#[derive(Debug)]
pub struct GuardImpl {
    mutex: *mut libc::pthread_mutex_t,
    cond: *mut libc::pthread_cond_t,
}

impl GuardImpl {
    #[inline]
    pub fn unlock(self) -> Result<(), LockError> {
        let mutex = ManuallyDrop::new(self).mutex;
        let r = unsafe { libc::pthread_mutex_unlock(mutex) };
        if r != 0 {
            return Err(LockError::InvalidState);
        }
        Ok(())
    }
}

impl<'g> RawGuard<'g> for GuardImpl {
    fn wait(self, timeout: Option<Instant>) -> Result<(Self, bool), LockError> {
        if let Some(exp) = timeout {
            if let Some(dur) = exp.checked_duration_since(Instant::now()) {
                if let Some(ts) = timeout_to_timespec(dur) {
                    let r = unsafe { libc::pthread_cond_timedwait(self.cond, self.mutex, &ts) };
                    if r == libc::ETIMEDOUT {
                        return Ok((self, true));
                    } else if r == 0 || (ts.tv_sec < 0 && r == libc::EINVAL) {
                        // some systems return EINVAL for a negative timeout. in
                        // those cases we just loop continuously
                        return Ok((self, false));
                    } else {
                        // FIXME cancel drop on self?
                        return Err(LockError::InvalidState);
                    }
                }
            // else: overflowed, just park indefinitely
            } else {
                return Ok((self, true));
            }
        }

        let r = unsafe { libc::pthread_cond_wait(self.cond, self.mutex) };
        if r != 0 {
            // FIXME cancel drop on self?
            return Err(LockError::InvalidState);
        }
        Ok((self, false))
    }
}

impl Drop for GuardImpl {
    #[inline]
    fn drop(&mut self) {
        let r = unsafe { libc::pthread_mutex_unlock(self.mutex) };
        assert_eq!(r, 0);
    }
}

pub struct ParkImpl {
    lock: LockImpl,
    state: AtomicUsize,
}

unsafe impl Send for ParkImpl {}
unsafe impl Sync for ParkImpl {}

impl RawInit for ParkImpl {
    #[inline]
    fn new() -> Self {
        Self {
            lock: LockImpl::new_uninit(),
            state: AtomicUsize::new(STATE_FREE),
        }
    }
}

impl RawParker for ParkImpl {
    #[inline]
    fn acquire(&self) -> Result<(), LockError> {
        if self.state.fetch_or(STATE_HELD, Ordering::Acquire) & STATE_HELD == 0 {
            Ok(())
        } else {
            Err(LockError::Contended)
        }
    }

    #[inline]
    fn release(&self) -> Result<(), LockError> {
        let found = self.state.fetch_and(!STATE_HELD, Ordering::Release);
        debug_assert!(found & STATE_HELD != 0);
        Ok(())
    }

    fn park(&self, timeout: Option<Instant>) -> Result<bool, LockError> {
        // relaxed ordering is suffient, we will check again after acquiring the mutex
        let mut state = self.state.load(Ordering::Relaxed);
        if state & STATE_UNPARK != 0 {
            // consume notification
            let found = self.state.swap(state & !STATE_UNPARK, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok(true);
        }
        if state & STATE_INIT == 0 {
            self.lock.init();
        }

        let mut guard = self.lock.lock()?;

        // relaxed is okay here because unpark will acquire the mutex before notifying
        state = self
            .state
            .fetch_or(STATE_INIT | STATE_PARK, Ordering::Relaxed)
            | STATE_INIT
            | STATE_PARK;
        if state & STATE_UNPARK != 0 {
            guard.unlock()?;
            let found = self.state.swap(state & !STATE_UNPARK, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok(true);
        }

        loop {
            let (g, timed_out) = guard.wait(timeout)?;
            guard = g;

            let state = self.state.load(Ordering::Acquire);
            if timed_out || state & STATE_UNPARK != 0 {
                let found = self
                    .state
                    .swap(state & !(STATE_PARK | STATE_UNPARK), Ordering::Acquire);
                guard.unlock()?;
                return Ok(found & STATE_UNPARK != 0);
            }
        }
    }

    fn unpark(&self) -> bool {
        let found = self.state.fetch_or(STATE_UNPARK, Ordering::Release);
        if found & STATE_UNPARK == 0 {
            if found & STATE_PARK != 0 {
                // acquire the mutex because the parking thread could be interrupted
                // between setting the state and waiting on the condvar
                drop(self.lock.lock().unwrap());
                // wake the parked thread
                self.lock.notify_one();
            }
            true
        } else {
            false
        }
    }
}

impl Debug for ParkImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("pthread::ParkImpl").finish()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
#[inline]
fn condvar_init(cvar: *mut libc::pthread_cond_t) {
    // Initializes the condvar to use CLOCK_MONOTONIC instead of CLOCK_REALTIME.
    let mut attr = MaybeUninit::<libc::pthread_condattr_t>::uninit();
    unsafe {
        let r = libc::pthread_condattr_init(attr.as_mut_ptr());
        assert_eq!(r, 0);
        let r = libc::pthread_condattr_setclock(attr.as_mut_ptr(), libc::CLOCK_MONOTONIC);
        assert_eq!(r, 0);
        let r = libc::pthread_cond_init(cvar, attr.as_ptr());
        assert_eq!(r, 0);
        let r = libc::pthread_condattr_destroy(attr.as_mut_ptr());
        assert_eq!(r, 0);
    }
}

#[inline]
fn mutex_init(mutex: *mut libc::pthread_mutex_t) {
    let mut attr = MaybeUninit::<libc::pthread_mutexattr_t>::uninit();
    unsafe {
        let r = libc::pthread_mutexattr_init(attr.as_mut_ptr());
        assert_eq!(r, 0);
        let r = libc::pthread_mutexattr_settype(attr.as_mut_ptr(), libc::PTHREAD_MUTEX_NORMAL);
        assert_eq!(r, 0);
        let r = libc::pthread_mutex_init(mutex, attr.as_ptr());
        assert_eq!(r, 0);
        let r = libc::pthread_mutexattr_destroy(attr.as_mut_ptr());
        assert_eq!(r, 0);
    }
}

// Returns the current time on the clock used by pthread_cond_t as a timespec.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[inline]
fn timespec_now() -> libc::timespec {
    let mut now = MaybeUninit::<libc::timeval>::uninit();
    let r = unsafe { libc::gettimeofday(now.as_mut_ptr(), ::core::ptr::null_mut()) };
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
