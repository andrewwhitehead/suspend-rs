//! Methods and primitives for waiting on notifications.

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
    task::{RawWaker, RawWakerVTable, Waker},
};

use crate::error::LockError;
use crate::raw_park::{NativeParker, RawParker};
use crate::types::{Expiry, ParkResult};
use crate::util::BoxPtr;

/// A structure providing thread parking and notification operations.
#[derive(Debug)]
pub struct Listener {
    inner: BoxPtr<ListenInner>,
}

impl Listener {
    /// Create a new `Listener` instance.
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: ListenInner::boxed(),
        }
    }

    /// Create a [`ListenerGuard`] from a shared reference to the `Listener`.
    /// This will fail with `LockError::Contended` if the `Listener` has
    /// already been locked.
    #[inline]
    pub fn lock(&self) -> Result<ListenerGuard<'_>, LockError> {
        unsafe { self.inner.to_ref() }.parker.acquire()?;
        Ok(ListenerGuard::new(self.inner, true))
    }

    /// Create a [`ListenerGuard`] from an exclusive reference to the `Listener`.
    /// This is more efficient than [`Listener::lock()`] because the type system
    /// ensures that no other references are in use.
    #[inline]
    pub fn get_mut(&mut self) -> ListenerGuard<'_> {
        ListenerGuard::new(self.inner, false)
    }

    /// Notify the `Listener` instance. This method returns `true` if the
    /// state was not already set to notified.
    #[inline]
    pub fn notify(&self) -> bool {
        unsafe { self.inner.to_ref() }.notify()
    }

    /// Create a new [`Notifier`] instance associated with this `Listener`.
    #[inline]
    pub fn notifier(&self) -> Notifier {
        unsafe { self.inner.to_ref() }.inc_count();
        Notifier { inner: self.inner }
    }

    /// Create a new [`Waker`] instance associated with this `Listener`.
    #[inline]
    pub fn waker(&self) -> Waker {
        ListenInner::waker(self.inner)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        ListenInner::dec_count_drop(self.inner);
    }
}

#[derive(Debug)]
pub(crate) struct ListenInner {
    pub(crate) count: AtomicUsize,
    pub(crate) parker: NativeParker,
}

impl ListenInner {
    const MAX_REFCOUNT: usize = (isize::MAX) as usize;

    const WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
        Self::waker_clone,
        Self::waker_wake,
        Self::waker_wake_by_ref,
        Self::waker_drop,
    );

    #[inline]
    pub fn boxed() -> BoxPtr<Self> {
        BoxPtr::alloc(Self::new())
    }

    pub const fn new() -> Self {
        Self::new_with_count(1)
    }

    pub const fn new_with_count(count: usize) -> Self {
        Self {
            count: AtomicUsize::new(count),
            parker: NativeParker::new(),
        }
    }

    #[inline]
    pub fn waker(inner: BoxPtr<Self>) -> Waker {
        unsafe {
            inner.to_ref().inc_count();
            Waker::from_raw(Self::raw_waker(inner.to_ptr()))
        }
    }

    #[inline]
    fn raw_waker(data: *const Self) -> RawWaker {
        RawWaker::new(data as *const (), &Self::WAKER_VTABLE)
    }

    unsafe fn waker_clone(data: *const ()) -> RawWaker {
        let inner = BoxPtr::from_ptr(data as *const Self);
        inner.to_ref().inc_count();
        Self::raw_waker(inner.to_ptr())
    }

    unsafe fn waker_wake(data: *const ()) {
        let inner = BoxPtr::from_ptr(data as *const Self);
        inner.to_ref().notify();
        Self::dec_count_drop(inner);
    }

    unsafe fn waker_wake_by_ref(data: *const ()) {
        (&*(data as *const Self)).notify();
    }

    unsafe fn waker_drop(data: *const ()) {
        let inner = BoxPtr::from_ptr(data as *const Self);
        Self::dec_count_drop(inner);
    }

    #[inline]
    pub fn inc_count(&self) {
        if self.count.fetch_add(1, Ordering::Relaxed) > Self::MAX_REFCOUNT {
            panic!("Exceeded max ref count");
        }
    }

    #[inline]
    pub fn dec_count(&self) -> usize {
        let count = self.count.fetch_sub(1, Ordering::Release) - 1;
        if count == 0 {
            // synchronize with updates
            self.count.load(Ordering::Acquire);
        }
        count
    }

    #[inline]
    pub fn dec_count_drop(inner: BoxPtr<Self>) {
        if unsafe { inner.to_ref() }.dec_count() == 0 {
            drop(unsafe { inner.dealloc() })
        }
    }

    pub fn notify(&self) -> bool {
        self.parker.unpark()
    }
}

/// An exclusive guard around a [`Listener`] instance.
#[derive(Debug)]
pub struct ListenerGuard<'g> {
    inner: BoxPtr<ListenInner>,
    release: bool,
    _marker: PhantomData<&'g ()>,
}

impl<'g> ListenerGuard<'g> {
    #[inline]
    pub(crate) fn new(inner: BoxPtr<ListenInner>, release: bool) -> Self {
        Self {
            inner,
            release,
            _marker: PhantomData,
        }
    }

    /// Park the current thread until the next notification, with an optional timeout.
    /// The timeout may be provided as an optional [`Instant`] or a
    /// [`Duration`](std::time::Duration).
    ///
    /// If the [`Listener`] instance has already been notified then this method will
    /// return immediately. The return value indicates whether a notification was
    /// consumed, returning `None` in the case of a timeout.
    pub fn park(&mut self, timeout: impl Into<Expiry>) -> Result<ParkResult, LockError> {
        unsafe { self.inner.to_ref() }.parker.park(timeout.into())
    }

    /// Create a [`Notifier`] associated with the [`Listener`] instance.
    #[inline]
    pub fn notifier(&self) -> Notifier {
        unsafe { self.inner.to_ref() }.inc_count();
        Notifier { inner: self.inner }
    }

    /// Create a [`Waker`] associated with the [`Listener`] instance.
    #[inline]
    pub fn waker(&self) -> Waker {
        ListenInner::waker(self.inner)
    }
}

impl Drop for ListenerGuard<'_> {
    fn drop(&mut self) {
        if self.release {
            unsafe { self.inner.to_ref() }.parker.release().unwrap();
        }
    }
}

/// Used to notify an associated [`Listener`] instance.
#[derive(Debug)]
pub struct Notifier {
    inner: BoxPtr<ListenInner>,
}

impl Clone for Notifier {
    fn clone(&self) -> Self {
        unsafe { self.inner.to_ref() }.inc_count();
        Self { inner: self.inner }
    }
}

impl Notifier {
    /// Notify the associated [`Listener`] instance, returning `true` if the
    /// the state was not already set to notified.
    #[inline]
    pub fn notify(&self) -> bool {
        unsafe { self.inner.to_ref() }.notify()
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        ListenInner::dec_count_drop(self.inner);
    }
}
