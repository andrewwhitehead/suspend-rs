//! Utilities for waiting on notifications.

use core::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};
use std::time::Instant;

use super::raw_lock::{Guard, LockError, NativeLock, RawLock};
use super::types::Expiry;

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
    let timeout: Option<Instant> = timeout.into_expire();
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
    let timeout: Option<Instant> = timeout.into_expire();
    block_on_poll(|cx| Pin::new(&mut fut).poll(cx), timeout)
}

/// Park the current thread until notified, with an optional timeout.
/// If a timeout is specified and reached before there is a notification, then
/// a `false` value is returned. Note that this function is susceptible to
/// spurious notifications. Use a dedicated [`Listener`] instance if this is
/// undesirable.
pub fn park_thread<'s>(f: impl FnOnce(Notifier), timeout: impl Expiry) -> bool {
    let timeout: Option<Instant> = timeout.into_expire();
    with_listener(|guard, _waker| {
        f(guard.notifier());
        guard.wait(timeout).unwrap()
    })
}

fn with_listener<T>(f: impl FnOnce(&mut ListenerGuard<'_>, &Waker) -> T) -> T {
    THREAD_LISTEN.with(|cached| {
        if let Ok(mut borrowed) = cached.try_borrow_mut() {
            let (listen, waker) = &mut *borrowed;
            let mut guard = listen.lock_mut();
            f(&mut guard, waker)
        } else {
            // thread listener in use, create a new one
            let mut listen = Listener::new();
            let mut guard = listen.lock_mut();
            let waker = guard.waker();
            f(&mut guard, &waker)
        }
    })
}

/// A structure providing thread parking and notification operations.
#[derive(Debug)]
pub struct Listener {
    inner: NonNull<Inner>,
}

impl Listener {
    /// Create a new `Listener` instance.
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(Inner::new()))) },
        }
    }

    /// Create a [`ListenerGuard`] from a reference to the `Listener`.
    /// This will fail with `LockError::Contended` if the `Listener` has
    /// already been locked.
    #[inline]
    pub fn lock(&self) -> Result<ListenerGuard<'_>, LockError> {
        unsafe {
            let guard = self.inner.as_ref().lock.lock()?;
            Ok(ListenerGuard::new(guard, self.inner))
        }
    }

    /// Create a [`ListenerGuard`] from a mutable reference to the `Listener`.
    /// This is more efficient than [`Listener::lock()`] because the type system
    /// ensures that no other references are in use.
    #[inline]
    pub fn lock_mut(&mut self) -> ListenerGuard<'_> {
        unsafe {
            let inner = self.inner;
            let guard = self.inner.as_mut().lock.lock_mut();
            ListenerGuard::new(guard, inner)
        }
    }

    /// Notify the `Listener` instance. This method returns `true` if the
    /// state was not already set to notified.
    #[inline]
    pub fn notify(&self) -> bool {
        unsafe { self.inner.as_ref() }.notify()
    }

    /// Create a new [`Notifier`] instance associated with this `Listener`.
    #[inline]
    pub fn notifier(&self) -> Notifier {
        unsafe { self.inner.as_ref() }.inc_count();
        Notifier { inner: self.inner }
    }

    /// Create a new [`Waker`] instance associated with this `Listener`.
    #[inline]
    pub fn waker(&self) -> Waker {
        Inner::waker(self.inner)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        unsafe { self.inner.as_ref() }.dec_count();
    }
}

pub(crate) struct Inner {
    count: AtomicUsize,
    lock: NativeLock,
}

impl Inner {
    const MAX_REFCOUNT: usize = (isize::MAX) as usize;

    const WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
        Self::waker_clone,
        Self::waker_wake,
        Self::waker_wake_by_ref,
        Self::waker_drop,
    );

    #[inline]
    pub fn new() -> Self {
        Self {
            count: AtomicUsize::new(1),
            lock: NativeLock::new(),
        }
    }

    #[inline]
    pub fn waker(inner: NonNull<Self>) -> Waker {
        unsafe {
            inner.as_ref().inc_count();
            Waker::from_raw(Self::raw_waker(inner.as_ptr()))
        }
    }

    #[inline]
    fn raw_waker(data: *const Self) -> RawWaker {
        RawWaker::new(data as *const (), &Self::WAKER_VTABLE)
    }

    unsafe fn waker_clone(data: *const ()) -> RawWaker {
        let inner = &*(data as *const Self);
        inner.inc_count();
        Self::raw_waker(data as *const Self)
    }

    unsafe fn waker_wake(data: *const ()) {
        let inner = &*(data as *const Self);
        inner.notify();
        inner.dec_count();
    }

    unsafe fn waker_wake_by_ref(data: *const ()) {
        let inner = &*(data as *const Self);
        inner.notify();
    }

    unsafe fn waker_drop(data: *const ()) {
        let inner = &*(data as *const Self);
        inner.dec_count();
    }

    #[inline]
    pub fn inc_count(&self) {
        if self.count.fetch_add(1, Ordering::Relaxed) > Self::MAX_REFCOUNT {
            panic!("Exceeded max listener ref count");
        }
    }

    pub fn dec_count(&self) {
        if self.count.fetch_sub(1, Ordering::Release) == 1 {
            self.count.load(Ordering::Acquire);
            unsafe { Box::from_raw(self as *const _ as *mut Self) };
        }
    }

    pub fn notify(&self) -> bool {
        self.lock.notify()
    }
}

/// An exclusive guard around a [`Listener`] instance.
#[derive(Debug)]
pub struct ListenerGuard<'s> {
    guard: Option<Guard<'s, NativeLock>>,
    inner: NonNull<Inner>,
}

impl<'s> ListenerGuard<'s> {
    #[inline]
    pub(crate) fn new(guard: Guard<'s, NativeLock>, inner: NonNull<Inner>) -> Self {
        Self {
            guard: Some(guard),
            inner,
        }
    }

    /// Create a [`Notifier`] associated with the [`Listener`] instance.
    #[inline]
    pub fn notifier(&self) -> Notifier {
        unsafe { self.inner.as_ref() }.inc_count();
        Notifier { inner: self.inner }
    }

    /// Create a [`Waker`] associated with the [`Listener`] instance.
    #[inline]
    pub fn waker(&self) -> Waker {
        Inner::waker(self.inner)
    }

    /// Park the current thread until the next notification, with an optional timeout.
    /// The timeout may be provided as an optional [`Instant`] or a
    /// [`Duration`](std::time::Duration).
    ///
    /// If the [`Listener`] instance has already been notified then this method will
    /// return immediately. The return value indicates whether a notification was
    /// consumed, returning `false` in the case of a timeout.
    pub fn wait(&mut self, timeout: impl Expiry) -> Result<bool, LockError> {
        if let Some(guard) = self.guard.take() {
            let inner = unsafe { self.inner.as_ref() };
            let (guard, notified) = inner.lock.park(guard, timeout.into_expire())?;
            self.guard.replace(guard);
            Ok(notified)
        } else {
            Err(LockError::Invalid)
        }
    }
}

/// Used to notify an associated [`Listener`] instance.
#[derive(Debug)]
pub struct Notifier {
    inner: NonNull<Inner>,
}

impl Clone for Notifier {
    fn clone(&self) -> Self {
        unsafe { self.inner.as_ref() }.inc_count();
        Self { inner: self.inner }
    }
}

unsafe impl Send for Notifier {}

impl Notifier {
    /// Notify the associated [`Listener`] instance, returning `true` if the
    /// the state was not already set to notified.
    #[inline]
    pub fn notify(&self) -> bool {
        unsafe { self.inner.as_ref() }.notify()
    }
}