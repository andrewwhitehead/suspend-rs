use std::{
    fmt::{self, Debug, Formatter},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Condvar, Mutex, PoisonError,
    },
    time::Instant,
};

use super::{LockError, RawLock};

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
        let state = self.state.load(Ordering::Relaxed);
        if state & STATE_NOTIFY != 0 {
            // consume notification
            let found = self.state.swap(state & !STATE_NOTIFY, Ordering::Acquire);
            debug_assert_eq!(found, state);
            return Ok(true);
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
                return Ok(true);
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
                return Ok(found & STATE_NOTIFY != 0);
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
