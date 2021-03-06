//! An Arc-like data structure which allows the owner to wait for
//! all outstanding borrows to be dropped.

use core::{
    borrow::Borrow,
    cell::UnsafeCell,
    fmt::{self, Debug, Formatter},
    future::Future,
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    ops::Deref,
    pin::Pin,
    sync::atomic::{fence, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use futures_core::future::FusedFuture;

use crate::util::BoxPtr;

#[cfg(feature = "std")]
use crate::{thread::block_on_poll, Expiry};

/// A shared value which can be borrowed by multiple threads and later
/// collected
#[derive(Debug)]
pub struct Lender<T> {
    inner: BoxPtr<SharedInner<T>>,
    pending: bool,
}

impl<T> Lender<T> {
    /// Construct a new `Lender<T>` instance
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            inner: SharedInner::boxed(value),
            pending: false,
        }
    }

    /// Obtain a mutable reference to the contained resource if there
    /// are no outstanding borrows
    pub fn get_mut(&mut self) -> Option<&mut T> {
        if self.try_collect() {
            Some(unsafe { self.get_mut_unchecked() })
        } else {
            None
        }
    }

    /// Obtain a mutable reference to the contained resource
    #[inline]
    pub unsafe fn get_mut_unchecked(&mut self) -> &mut T {
        (&mut *(self.inner.to_ptr() as *mut SharedInner<T>)).as_mut()
    }

    /// Unwrap the `Lender<T>` when there are no outstanding references
    #[inline]
    pub unsafe fn into_inner(self) -> T {
        self.inner.into_box().into_inner()
    }

    /// Create a new shared reference to the contained value
    #[inline]
    pub fn borrow(&self) -> Shared<T> {
        Shared::new(self.inner)
    }

    /// Create a new shared reference to the contained value
    #[inline]
    pub fn borrow_ref(&self) -> SharedRef<'_, T> {
        SharedRef {
            inner: self.inner,
            _marker: PhantomData,
        }
    }

    /// Get the current number of borrows
    #[inline]
    pub fn borrow_count(&self) -> usize {
        unsafe { self.inner.to_ref() }.borrow_count()
    }

    /// Wait for all outstanding borrows to be dropped, with an optional expiry
    pub fn collect(&mut self) -> Collect<'_, T> {
        Collect(Some(self))
    }

    /// Wait for all outstanding borrows to be dropped and unwrap the `Shared<T>`
    pub fn collect_into(self) -> CollectInto<T> {
        CollectInto(Some(self))
    }

    /// Obtain an owned copy of the lent resource
    pub fn to_owned(&self) -> T
    where
        T: Clone,
    {
        self.as_ref().clone()
    }

    fn poll_collect(&mut self, waker: &Waker) -> Poll<()> {
        let result = if self.pending {
            unsafe { self.inner.to_ref() }.resume_poll(waker)
        } else {
            unsafe { self.inner.to_ref() }.start_poll(waker)
        };
        self.pending = result.is_pending();
        result
    }

    fn try_collect(&mut self) -> bool {
        if unsafe { self.inner.to_ref() }.try_collect(self.pending) {
            self.pending = false;
            true
        } else {
            false
        }
    }

    fn cancel_collect(&mut self) {
        if self.pending {
            unsafe { self.inner.to_ref() }.cancel_poll(true);
        }
    }
}

impl<T> AsRef<T> for Lender<T> {
    #[inline]
    fn as_ref(&self) -> &T {
        unsafe { self.inner.to_ref() }.as_ref()
    }
}

impl<T> Borrow<T> for Lender<T> {
    #[inline]
    fn borrow(&self) -> &T {
        unsafe { self.inner.to_ref() }.as_ref()
    }
}

impl<T> Drop for Lender<T> {
    fn drop(&mut self) {
        unsafe {
            SharedInner::drop_lender(self.inner, self.pending);
        }
    }
}

impl<T> PartialEq for Lender<T> {
    fn eq(&self, other: &Lender<T>) -> bool {
        other.inner == self.inner
    }
}

pub(crate) struct SharedInner<T> {
    count: AtomicUsize,
    waker: UnsafeCell<MaybeUninit<Waker>>,
    value: T,
}

impl<T> SharedInner<T> {
    const MAX_REFCOUNT: usize = (isize::MAX) as usize;

    #[inline]
    pub fn boxed(value: T) -> BoxPtr<Self> {
        BoxPtr::alloc(Self::new(value))
    }

    pub const fn new(value: T) -> Self {
        Self {
            count: AtomicUsize::new(3),
            waker: UnsafeCell::new(MaybeUninit::uninit()),
            value,
        }
    }

    #[inline]
    pub unsafe fn into_inner(self) -> T {
        self.value
    }

    #[inline]
    pub fn borrow_count(&self) -> usize {
        (self.count.load(Ordering::Relaxed) / 2).saturating_sub(1)
    }

    #[inline]
    pub unsafe fn drop_lender(slf: BoxPtr<Self>, pending: bool) {
        if (pending && slf.to_ref().cancel_poll(false))
            || slf.to_ref().count.fetch_sub(3, Ordering::Release) == 3
        {
            fence(Ordering::Acquire);
            slf.dealloc();
        }
    }

    #[inline]
    pub unsafe fn inc_count_ref(slf: *const Self) {
        if (&*slf).count.fetch_add(2, Ordering::Relaxed) > Self::MAX_REFCOUNT {
            panic!("Exceeded max ref count");
        }
    }

    pub unsafe fn dec_count_ref(slf: *const Self) -> bool {
        let inner = &*slf;
        // decrease count by two, each reference counts twice
        loop {
            match inner.count.fetch_sub(2, Ordering::Release) - 2 {
                0 => {
                    // Shared has been dropped and this was the last reference
                    // be sure to synchronize with the last drop
                    fence(Ordering::Acquire);
                    return true;
                }
                1 => {
                    fence(Ordering::Acquire);
                    // Shared is in collection phase and this was the last reference.
                    // it will wait for us to readjust the count, so that the
                    // allocation is not dropped before we can notify it
                    inner.wake_waker();
                    // inner may have given up waiting, in which case the update will fail
                    if inner
                        .count
                        .compare_exchange(1, 2, Ordering::Release, Ordering::Relaxed)
                        .is_ok()
                    {
                        return false;
                    }
                    // otherwise we were interrupted, retry
                }
                _ => return false,
            }
        }
    }

    #[inline]
    unsafe fn clear_waker(&self) {
        (&mut *self.waker.get()).as_mut_ptr().drop_in_place();
    }

    #[inline]
    unsafe fn store_waker(&self, waker: &Waker) {
        self.waker.get().write(MaybeUninit::new(waker.clone()));
    }

    #[inline]
    unsafe fn wake_waker(&self) {
        self.waker.get().read().assume_init().wake();
    }

    pub fn try_collect(&self, pending: bool) -> bool {
        // be sure to synchronize with the last dropped ref
        let count = self.count.load(Ordering::Relaxed);
        if pending && count == 2 {
            assert_eq!(2, self.count.swap(3, Ordering::Acquire));
            true
        } else if !pending && count == 3 {
            fence(Ordering::Acquire);
            true
        } else {
            false
        }
    }

    pub fn start_poll(&self, waker: &Waker) -> Poll<()> {
        let mut count = self.count.load(Ordering::Relaxed);
        if count == 3 {
            // be sure to synchronize with the last dropped ref
            fence(Ordering::Acquire);
            return Poll::Ready(());
        }

        unsafe { self.store_waker(waker) };

        count = self.count.fetch_sub(2, Ordering::Release) - 2;
        if count == 1 {
            // last reference was dropped between acquiring and updating the count
            unsafe { self.clear_waker() };
            // use acquire here to synchronize with the drop
            count = self.count.swap(3, Ordering::Acquire);
            assert_eq!(count, 1, "Invalid shared state");
            return Poll::Ready(());
        }

        Poll::Pending
    }

    pub fn resume_poll(&self, waker: &Waker) -> Poll<()> {
        let mut count = self.count.load(Ordering::Relaxed);
        loop {
            match count {
                2 => {
                    // last reference has been dropped and waker has been cleared
                    fence(Ordering::Acquire);
                    return Poll::Ready(());
                }
                1 => {
                    // last reference was interrupted while notifying us
                    // yield to let it complete
                    waker.wake_by_ref();
                    return Poll::Pending;
                }
                _ => {
                    match self.count.compare_exchange_weak(
                        count,
                        count + 2,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            unsafe {
                                self.clear_waker();
                                self.store_waker(waker);
                            }
                            count = self.count.fetch_sub(2, Ordering::Release) - 2;
                            if count == 2 {
                                unsafe { self.clear_waker() };
                            }
                        }
                        Err(c) => {
                            count = c;
                        }
                    }
                }
            }
        }
    }

    // returns is-last-reference
    pub fn cancel_poll(&self, reset: bool) -> bool {
        let mut count = self.count.load(Ordering::Relaxed);
        loop {
            match count {
                2 => {
                    // last reference dropped during collection
                    if reset {
                        assert_eq!(2, self.count.swap(3, Ordering::Relaxed));
                    }
                    return true;
                }
                1 => {
                    // last reference interrupted mid-drop. it will call the
                    // stored waker and then attempt to change the refcount to
                    // 2. if that fails, it will retry
                    match self.count.compare_exchange_weak(
                        1,
                        if reset { 5 } else { 2 },
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            // last reference is now responsible for dropping the allocation
                            return !reset;
                        }
                        Err(c) => {
                            count = c;
                        }
                    }
                }
                s => {
                    // try increase reference count so it is safe to clear the waker
                    match self.count.compare_exchange_weak(
                        s,
                        s + 2,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            // stored waker is now safe to remove
                            unsafe { self.clear_waker() };
                            return false;
                        }
                        Err(c) => {
                            count = c;
                        }
                    }
                }
            }
        }
    }
}

impl<T> AsRef<T> for SharedInner<T> {
    #[inline]
    fn as_ref(&self) -> &T {
        &self.value
    }
}

impl<T> AsMut<T> for SharedInner<T> {
    #[inline]
    fn as_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T: Debug> Debug for SharedInner<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedInner")
            .field("value", &self.value)
            .finish()
    }
}

unsafe impl<T> Sync for SharedInner<T> {}

/// A `Future` which resolves when all outstanding borrows of the `Lender<T>`
/// have been returned
#[derive(Debug)]
pub struct Collect<'c, T>(Option<&'c mut Lender<T>>);

impl<'c, T> Collect<'c, T> {
    /// Resolve or abort the collection of the resource
    pub fn resolve(self) -> Result<&'c mut T, &'c mut Lender<T>> {
        let lender = ManuallyDrop::new(self)
            .0
            .take()
            .expect("Cannot call Collect::resolve() after polling");
        if lender.try_collect() {
            return Ok(unsafe { lender.get_mut_unchecked() });
        }
        return Err(lender);
    }

    #[cfg(feature = "std")]
    /// Block the current thread on the collection of the shared resource
    pub fn wait(self, timeout: impl Into<Expiry>) -> Result<&'c mut T, &'c mut Lender<T>> {
        let lender = ManuallyDrop::new(self)
            .0
            .take()
            .expect("Cannot call Collect::wait() after polling");
        if lender.try_collect() {
            return Ok(unsafe { lender.get_mut_unchecked() });
        }
        match block_on_poll(|cx| lender.poll_collect(cx.waker()), timeout) {
            Poll::Ready(()) => Ok(unsafe { lender.get_mut_unchecked() }),
            Poll::Pending => Err(lender),
        }
    }
}

impl<T> Drop for Collect<'_, T> {
    fn drop(&mut self) {
        if self.0.is_some() {
            self.0.take().unwrap().cancel_collect();
        }
    }
}

impl<'c, T> Future for Collect<'c, T> {
    type Output = &'c mut T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(lender) = self.0.take() {
            match lender.poll_collect(cx.waker()) {
                Poll::Ready(()) => Poll::Ready(unsafe { lender.get_mut_unchecked() }),
                Poll::Pending => {
                    self.0.replace(lender);
                    Poll::Pending
                }
            }
        } else {
            Poll::Pending
        }
    }
}

impl<T> FusedFuture for Collect<'_, T> {
    fn is_terminated(&self) -> bool {
        self.0.is_none()
    }
}

impl<T> Unpin for Collect<'_, T> {}

/// A `Future` which resolves when all outstanding borrows of the `Lender<T>`
/// have been returned
#[derive(Debug)]
pub struct CollectInto<T>(Option<Lender<T>>);

impl<T> CollectInto<T> {
    /// Resolve or abort the collection of the resource
    pub fn resolve(self) -> Result<T, Lender<T>> {
        let mut lender = ManuallyDrop::new(self)
            .0
            .take()
            .expect("Cannot call CollectInto::resolve() after polling");
        if lender.try_collect() {
            return Ok(unsafe { lender.into_inner() });
        }
        return Err(lender);
    }

    #[cfg(feature = "std")]
    /// Block the current thread on the collection of the shared resource
    pub fn wait(self, timeout: impl Into<Expiry>) -> Result<T, Lender<T>> {
        let mut lender = ManuallyDrop::new(self)
            .0
            .take()
            .expect("Cannot call CollectInto::wait() after polling");
        if lender.try_collect() {
            return Ok(unsafe { lender.into_inner() });
        }
        match block_on_poll(|cx| lender.poll_collect(cx.waker()), timeout) {
            Poll::Ready(()) => Ok(unsafe { lender.into_inner() }),
            Poll::Pending => Err(lender),
        }
    }
}

impl<T> Future for CollectInto<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(mut lender) = self.0.take() {
            match lender.poll_collect(cx.waker()) {
                Poll::Ready(()) => Poll::Ready(unsafe { lender.into_inner() }),
                Poll::Pending => {
                    self.0.replace(lender);
                    Poll::Pending
                }
            }
        } else {
            Poll::Pending
        }
    }
}

impl<T> FusedFuture for CollectInto<T> {
    fn is_terminated(&self) -> bool {
        self.0.is_none()
    }
}

impl<T> Unpin for CollectInto<T> {}

/// A tracking value which notifies its source when dropped
#[derive(Debug)]
pub struct Shared<T> {
    inner: BoxPtr<SharedInner<T>>,
}

impl<T> Shared<T> {
    pub(crate) fn new(inner: BoxPtr<SharedInner<T>>) -> Self {
        unsafe { SharedInner::inc_count_ref(inner.to_ptr()) };
        Shared { inner }
    }

    /// Get the current number of borrows, including this one
    #[inline]
    pub fn borrow_count(&self) -> usize {
        unsafe { self.inner.to_ref() }.borrow_count()
    }

    /// Create a temporary reference without increasing the borrow count
    pub const fn borrow_ref(&self) -> SharedRef<'_, T> {
        SharedRef {
            inner: self.inner,
            _marker: PhantomData,
        }
    }
}

impl<T> AsRef<T> for Shared<T> {
    #[inline]
    fn as_ref(&self) -> &T {
        unsafe { self.inner.to_ref() }.as_ref()
    }
}

impl<T> Borrow<T> for Shared<T> {
    #[inline]
    fn borrow(&self) -> &T {
        unsafe { self.inner.to_ref() }.as_ref()
    }
}

impl<T> Clone for Shared<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self::new(self.inner)
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { self.inner.to_ref() }.as_ref()
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        unsafe {
            if SharedInner::dec_count_ref(self.inner.to_ptr()) {
                self.inner.dealloc()
            }
        }
    }
}

/// A temporary reference to a shared value
#[derive(Debug)]
#[repr(transparent)]
pub struct SharedRef<'s, T> {
    inner: BoxPtr<SharedInner<T>>,
    _marker: PhantomData<&'s ()>,
}

impl<T> SharedRef<'_, T> {
    /// Get the current number of borrows, including this one
    #[inline]
    pub fn borrow_count(&self) -> usize {
        unsafe { self.inner.to_ref() }.borrow_count()
    }

    /// Construct a new `Shared<T>` for the associated `Shared<T>`
    #[inline]
    pub fn into_shared(self) -> Shared<T> {
        Shared::new(self.inner)
    }
}

impl<T> AsRef<T> for SharedRef<'_, T> {
    #[inline]
    fn as_ref(&self) -> &T {
        unsafe { self.inner.to_ref() }.as_ref()
    }
}

impl<T> Borrow<T> for SharedRef<'_, T> {
    #[inline]
    fn borrow(&self) -> &T {
        unsafe { self.inner.to_ref() }.as_ref()
    }
}

impl<T> Clone for SharedRef<'_, T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner,
            _marker: PhantomData,
        }
    }
}
impl<T> Copy for SharedRef<'_, T> {}

impl<T> Deref for SharedRef<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { self.inner.to_ref() }.as_ref()
    }
}
