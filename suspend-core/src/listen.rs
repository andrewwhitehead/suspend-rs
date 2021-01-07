//! Methods and primitives for waiting on notifications.

use core::{
    cell::RefCell,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};
use std::time::Instant;

use crate::error::LockError;
use crate::raw_lock::{NativeParker, RawInit, RawParker};
use crate::types::Expiry;
use crate::util::BoxPtr;

thread_local! {
    pub(crate) static THREAD_LISTEN: RefCell<(Listener, Waker)> = {
        let listener = Listener::new();
        let waker = listener.waker();
        RefCell::new((listener, waker))
    };
}

/// Block the current thread on the result of a [`Future`].
pub fn block_on<'s, T>(fut: impl Future<Output = T>) -> T {
    pin!(fut);
    with_listener(|guard, waker| {
        let mut cx = Context::from_waker(waker);
        loop {
            if let Poll::Ready(result) = fut.as_mut().poll(&mut cx) {
                break result;
            }
            guard.wait(None).unwrap();
        }
    })
}

/// Block the current thread on the result of a poll function, with an optional timeout.
/// A [`None`] value is returned if the timeout is reached.
pub fn block_on_poll<'s, T>(
    mut poll: impl FnMut(&mut Context<'_>) -> Poll<T>,
    timeout: impl Expiry,
) -> Poll<T> {
    let timeout: Option<Instant> = timeout.into_opt_instant();
    with_listener(|guard, waker| {
        let mut cx = Context::from_waker(waker);
        loop {
            let result = poll(&mut cx);
            if result.is_ready() || !guard.wait(timeout).unwrap() {
                break result;
            }
        }
    })
}

/// Block the current thread on the result of a [`Future`] + [`Unpin`], with an
/// optional timeout. A [`None`] value is returned if the timeout is reached.
#[inline]
pub fn block_on_unpin<'s, T>(
    mut fut: impl Future<Output = T> + Unpin,
    timeout: impl Expiry,
) -> Poll<T> {
    let timeout: Option<Instant> = timeout.into_opt_instant();
    block_on_poll(|cx| Pin::new(&mut fut).poll(cx), timeout)
}

/// Park the current thread until notified, with an optional timeout.
/// If a timeout is specified and reached before there is a notification, then
/// a `false` value is returned. Note that this function is susceptible to
/// spurious notifications. Use a dedicated [`Listener`] instance if this is
/// undesirable.
pub fn park_thread<'s>(f: impl FnOnce(Notifier), timeout: impl Expiry) -> bool {
    let timeout: Option<Instant> = timeout.into_opt_instant();
    with_listener(|guard, _waker| {
        f(guard.notifier());
        guard.wait(timeout).unwrap()
    })
}

fn with_listener<T>(f: impl FnOnce(&mut ListenerGuard<'_>, &Waker) -> T) -> T {
    THREAD_LISTEN.with(|cached| {
        if let Ok(mut borrowed) = cached.try_borrow_mut() {
            let (listen, waker) = &mut *borrowed;
            let mut guard = listen.get_mut();
            f(&mut guard, waker)
        } else {
            // thread listener in use, create a new one
            let mut listen = Listener::new();
            let mut guard = listen.get_mut();
            let waker = guard.waker();
            f(&mut guard, &waker)
        }
    })
}

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

    #[inline]
    pub fn new() -> Self {
        Self::new_with_count(1)
    }

    #[inline]
    pub fn new_with_count(count: usize) -> Self {
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

    /// Park the current thread until the next notification, with an optional timeout.
    /// The timeout may be provided as an optional [`Instant`] or a
    /// [`Duration`](std::time::Duration).
    ///
    /// If the [`Listener`] instance has already been notified then this method will
    /// return immediately. The return value indicates whether a notification was
    /// consumed, returning `false` in the case of a timeout.
    pub fn wait(&mut self, timeout: impl Expiry) -> Result<bool, LockError> {
        unsafe { self.inner.to_ref() }
            .parker
            .park(timeout.into_opt_instant())
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
