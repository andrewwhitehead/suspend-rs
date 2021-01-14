use std::{
    fmt::{self, Debug, Formatter},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Condvar, Mutex, MutexGuard, PoisonError, TryLockError,
    },
    time::Instant,
};

use super::{HasGuard, LockError, RawGuard, RawInit, RawLock, RawParker};

const STATE_FREE: usize = 0b0000;
const STATE_HELD: usize = 0b0010;
const STATE_PARK: usize = 0b0100;
const STATE_UNPARK: usize = 0b1000;

impl<T> From<PoisonError<T>> for LockError {
    fn from(_: PoisonError<T>) -> Self {
        LockError::Poisoned
    }
}

pub struct LockImpl {
    cond: Condvar,
    mutex: Mutex<()>,
}

impl RawInit for LockImpl {
    #[inline]
    fn new() -> Self {
        Self {
            cond: Condvar::new(),
            mutex: Mutex::new(()),
        }
    }
}

impl RawLock for LockImpl {
    #[inline]
    fn lock(&self) -> Result<GuardImpl<'_>, LockError> {
        let guard = self.mutex.lock()?;
        Ok(GuardImpl {
            guard,
            cond: &self.cond,
        })
    }

    #[inline]
    fn try_lock(&self) -> Result<<Self as HasGuard<'_>>::Guard, LockError> {
        match self.mutex.try_lock() {
            Ok(guard) => Ok(GuardImpl {
                guard,
                cond: &self.cond,
            }),
            Err(TryLockError::Poisoned(..)) => Err(LockError::Poisoned),
            Err(TryLockError::WouldBlock) => Err(LockError::Contended),
        }
    }

    #[inline]
    fn notify_one(&self) {
        self.cond.notify_one();
    }

    #[inline]
    fn notify_all(&self) {
        self.cond.notify_all();
    }
}

impl Debug for LockImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("default::LockImpl").finish()
    }
}

impl<'g> HasGuard<'g> for LockImpl {
    type Guard = GuardImpl<'g>;
}

#[derive(Debug)]
pub struct GuardImpl<'g> {
    guard: MutexGuard<'g, ()>,
    cond: &'g Condvar,
}

impl<'g> RawGuard<'g> for GuardImpl<'g> {
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
    cond: Condvar,
    mutex: Mutex<()>,
    state: AtomicUsize,
}

impl RawInit for ParkImpl {
    #[inline]
    fn new() -> Self {
        Self {
            cond: Condvar::new(),
            mutex: Mutex::new(()),
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

    fn park(&self, timeout: Option<Instant>) -> Result<Option<bool>, LockError> {
        // relaxed ordering is suffient, we will check again after acquiring the mutex
        let state = self.state.load(Ordering::Relaxed);
        if state & STATE_UNPARK != 0 {
            // consume notification
            let found = self.state.swap(state & !STATE_UNPARK, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok(Some(false));
        }

        let mut guard = GuardImpl {
            guard: self.mutex.lock()?,
            cond: &self.cond,
        };

        match self.state.compare_exchange(
            state,
            state | STATE_PARK,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(_) => (),
            Err(state) if state & STATE_UNPARK != 0 => {
                drop(guard);
                let found = self.state.swap(state & !STATE_UNPARK, Ordering::Acquire);
                debug_assert_eq!(found, state);
                return Ok(Some(false));
            }
            _ => panic!("Invalid lock state update"),
        }

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
                    Some(true)
                } else {
                    None
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
                drop(self.mutex.lock());
                // wake the parked thread
                self.cond.notify_one();
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
