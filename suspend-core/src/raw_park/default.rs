use std::{
    cell::UnsafeCell,
    fmt::{self, Debug, Formatter},
    mem::MaybeUninit,
    ptr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Condvar, Mutex, MutexGuard, PoisonError,
    },
    time::Instant,
};

use super::{LockError, RawParker};
use crate::types::{Expiry, ParkResult};

const STATE_FREE: usize = 0b0000;
const STATE_INIT: usize = 0b0001;
const STATE_HELD: usize = 0b0010;
const STATE_PARK: usize = 0b0100;
const STATE_UNPARK: usize = 0b1000;

impl<T> From<PoisonError<T>> for LockError {
    fn from(_: PoisonError<T>) -> Self {
        LockError::Poisoned
    }
}

#[derive(Debug)]
pub struct GuardImpl<'g> {
    guard: MutexGuard<'g, ()>,
    cond: &'g Condvar,
}

impl GuardImpl<'_> {
    fn wait(self, timeout: Option<Instant>) -> Result<(Self, bool), LockError> {
        if let Some(exp) = timeout {
            if let Some(dur) = exp.checked_duration_since(Instant::now()) {
                let (guard, timeout_result) = self.cond.wait_timeout(self.guard, dur)?;
                Ok((
                    Self {
                        guard,
                        cond: self.cond,
                    },
                    timeout_result.timed_out(),
                ))
            } else {
                Ok((self, true))
            }
        } else {
            let guard = self.cond.wait(self.guard)?;
            Ok((
                Self {
                    guard,
                    cond: self.cond,
                },
                false,
            ))
        }
    }
}

pub struct ParkImpl {
    cond: UnsafeCell<MaybeUninit<Condvar>>,
    mutex: UnsafeCell<MaybeUninit<Mutex<()>>>,
    state: AtomicUsize,
}

unsafe impl Send for ParkImpl {}
unsafe impl Sync for ParkImpl {}

impl ParkImpl {
    #[allow(unused)]
    pub const fn new() -> Self {
        Self {
            cond: UnsafeCell::new(MaybeUninit::uninit()),
            mutex: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(STATE_FREE),
        }
    }

    #[inline]
    fn lock_init(&self) {
        unsafe {
            *self.cond.get() = MaybeUninit::new(Condvar::new());
            *self.mutex.get() = MaybeUninit::new(Mutex::new(()));
        }
    }

    fn lock_destroy(&self) {
        unsafe {
            ptr::drop_in_place((&mut *self.cond.get()).as_mut_ptr());
            ptr::drop_in_place((&mut *self.mutex.get()).as_mut_ptr());
        }
    }

    fn lock_acquire(&self) -> Result<GuardImpl<'_>, LockError> {
        let cond = unsafe { &*(&*self.cond.get()).as_ptr() };
        let mutex = unsafe { &*(&*self.mutex.get()).as_ptr() };
        Ok(GuardImpl {
            guard: mutex.lock()?,
            cond,
        })
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

    fn park(&self, timeout: Expiry) -> Result<ParkResult, LockError> {
        // relaxed ordering is suffient, we will check again after acquiring the mutex
        let mut state = self.state.load(Ordering::Relaxed);
        if state & STATE_UNPARK != 0 {
            // consume notification
            let found = self.state.swap(state & !STATE_UNPARK, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok(ParkResult::Skipped);
        }

        struct InitGuard<'g>(&'g mut ParkImpl);
        impl Drop for InitGuard<'_> {
            fn drop(&mut self) {
                self.0.lock_destroy();
            }
        }
        let inited = if state & STATE_INIT == 0 {
            self.lock_init();
            Some(InitGuard)
        } else {
            None
        };

        let mut guard = self.lock_acquire()?;
        drop(inited);

        state = self.state.fetch_or(STATE_PARK, Ordering::Relaxed) | STATE_PARK;
        if state & STATE_UNPARK != 0 {
            drop(guard);
            let found = self.state.swap(state & !STATE_UNPARK, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok(ParkResult::Skipped);
        }

        let timeout = timeout.into_opt_instant();
        loop {
            let (g, timed_out) = guard.wait(timeout)?;
            guard = g;

            let state = self.state.load(Ordering::Acquire);
            if timed_out || state & STATE_UNPARK != 0 {
                let found = self
                    .state
                    .swap(state & !(STATE_PARK | STATE_UNPARK), Ordering::Acquire);
                drop(guard);
                return Ok(if found & STATE_UNPARK != 0 {
                    ParkResult::Unparked
                } else {
                    ParkResult::TimedOut
                });
            }
        }
    }

    fn unpark(&self) -> bool {
        let found = self.state.fetch_or(STATE_UNPARK, Ordering::Release);
        if found & STATE_UNPARK == 0 {
            if found & STATE_PARK != 0 {
                // acquire the mutex because the parking thread could be interrupted
                // between setting the state and waiting on the condvar
                drop(self.lock_acquire());
                // wake the parked thread
                unsafe { &*(&*self.cond.get()).as_ptr() }.notify_one();
            }
            true
        } else {
            false
        }
    }
}

impl Debug for ParkImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("default::ParkImpl").finish()
    }
}
