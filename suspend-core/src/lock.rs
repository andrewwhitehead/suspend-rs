//! Platform-independent locking primitives

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
};

use crate::{
    raw_lock::{HasGuard, NativeLock, RawGuard, RawInit, RawLock},
    Expiry, LockError,
};

/// A combination of a Mutex<T> and Condvar
#[derive(Debug)]
pub struct Lock<T: ?Sized> {
    raw: NativeLock,
    value: UnsafeCell<T>,
}

impl<T> Lock<T> {
    /// Create a new `Lock<T>`
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            raw: NativeLock::new(),
            value: value.into(),
        }
    }

    /// Acquire exclusive access to the contained value
    pub fn lock(&self) -> Result<LockGuard<'_, T>, LockError> {
        let guard = self.raw.lock()?;
        Ok(LockGuard {
            guard: Some(guard),
            lock: self,
        })
    }

    /// Acquire exclusive access to the contained value
    pub fn try_lock(&self) -> Result<LockGuard<'_, T>, LockError> {
        let guard = self.raw.try_lock()?;
        Ok(LockGuard {
            guard: Some(guard),
            lock: self,
        })
    }

    /// Unwrap the Lock<T>, returning the inner value
    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

unsafe impl<T: Send> Sync for Lock<T> {}

impl<T: Default> Default for Lock<T> {
    /// Creates a `Lock`, with the `Default` value for T
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T: Send> From<T> for Lock<T> {
    fn from(t: T) -> Lock<T> {
        Lock::new(t)
    }
}

/// An RAII guard around a `Lock<T>`
#[derive(Debug)]
pub struct LockGuard<'g, T> {
    guard: Option<<NativeLock as HasGuard<'g>>::Guard>,
    lock: &'g Lock<T>,
}

impl<'g, T> LockGuard<'g, T> {
    /// Wait on a lock guard with an optional timeout
    pub fn wait(mut self, timeout: impl Expiry) -> Result<(LockGuard<'g, T>, bool), LockError> {
        let expire = timeout.into_opt_instant();
        let guard = self.guard.take().unwrap();
        let (guard, timed_out) = guard.wait(expire)?;
        self.guard.replace(guard);
        Ok((self, timed_out))
    }
}

impl<T> Deref for LockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.value.get() }
    }
}
