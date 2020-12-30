use std::{
    fmt::{self, Debug, Formatter},
    ptr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Condvar, Mutex, PoisonError,
    },
    time::Instant,
};

use super::{Guard, HasGuard, LockError, RawLock};

impl<T> From<PoisonError<T>> for LockError {
    fn from(_: PoisonError<T>) -> Self {
        LockError::Poisoned
    }
}

const STATE_FREE: usize = 0b0000;
const STATE_HELD: usize = 0b0001;
const STATE_PARK: usize = 0b0010;
const STATE_NOTIFY: usize = 0b0100;

pub struct LockImpl {
    cond: Condvar,
    mutex: Mutex<()>,
    state: AtomicUsize,
}

impl RawLock for LockImpl {
    #[inline]
    fn new() -> Self {
        Self {
            cond: Condvar::new(),
            mutex: Mutex::new(()),
            state: AtomicUsize::new(STATE_FREE),
        }
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

        let mut mlock = self.mutex.lock()?;

        match self.state.compare_exchange(
            state,
            state | STATE_PARK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => (),
            Err(state) if state & STATE_NOTIFY != 0 => {
                drop(mlock);
                let found = self.state.swap(state & !STATE_NOTIFY, Ordering::Acquire);
                debug_assert_eq!(found, state);
                return Ok((guard, true));
            }
            _ => panic!("Invalid lock state update"),
        }

        loop {
            let timeout_state = if let Some(exp) = timeout {
                if let Some(dur) = exp.checked_duration_since(Instant::now()) {
                    let (ml, timeout_result) = self.cond.wait_timeout(mlock, dur)?;
                    mlock = ml;
                    Some(timeout_result.timed_out())
                } else {
                    Some(true)
                }
            } else {
                None
            };

            let timed_out = if let Some(t) = timeout_state {
                t
            } else {
                mlock = self.cond.wait(mlock)?;
                false
            };

            let state = self.state.load(Ordering::Relaxed);
            if timed_out || state & STATE_NOTIFY != 0 {
                let found = self
                    .state
                    .swap(state & !(STATE_PARK | STATE_NOTIFY), Ordering::Acquire);
                drop(mlock);
                return Ok((guard, found & STATE_NOTIFY != 0));
            }
        }
    }

    fn notify(&self) -> bool {
        let found = self.state.fetch_or(STATE_NOTIFY, Ordering::Release);
        if found & STATE_NOTIFY == 0 {
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

impl Debug for LockImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("default::LockImpl").finish()
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
