//! An Arc-like data structure which allows the owner to wait for
//! all outstanding borrows to be dropped.

use core::{
    future::Future,
    marker::PhantomData,
    mem::ManuallyDrop,
    ops::Deref,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
};
use std::time::Instant;

use crate::{
    error::LockError,
    raw_lock::{NativeParker, RawInit, RawParker},
    types::Expiry,
    util::BoxPtr,
};

/// Run a function and then block the current thread until all trackers
/// produced by the function have been dropped. This can be used to
/// track the completion of spawned threads or futures.
pub fn with_scope<F, R>(f: F) -> R
where
    F: FnOnce(ScopedRef<()>) -> R,
{
    let mut scope = PinShared::new(());
    scope.with(f)
}

/// Create a `Future` that collects all trackers from the scope when it
/// is dropped.
pub async fn scoped_future<'s, F, Fut, R>(f: F) -> R
where
    F: FnOnce(ScopedRef<()>) -> Fut,
    Fut: Future<Output = R>,
{
    let mut scope = PinShared::new(());
    scope.async_with(f).await
}

/// A shared value which can be borrowed by multiple threads and later
/// collected
#[derive(Debug)]
pub struct Shared<T> {
    inner: BoxPtr<SharedInner<T>>,
}

impl<T> Shared<T> {
    /// Construct a new `Shared<T>` instance
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            inner: SharedInner::boxed(value),
        }
    }

    /// Create a new shared reference to the contained value
    #[inline]
    pub fn borrow(&self) -> SharedRef<T> {
        SharedRef::new(self.inner)
    }

    /// Create a temporary reference without increasing the borrow count
    pub const fn scoped_ref(&self) -> Ref<'_, T> {
        Ref {
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
    pub fn collect(&mut self, timeout: impl Expiry) -> Result<bool, LockError> {
        unsafe { self.inner.to_ref() }.collect(timeout.into_opt_instant())
    }

    /// Wait for all outstanding borrows to be dropped and unwrap the `Shared<T>`
    pub fn collect_into(self) -> Result<T, LockError> {
        unsafe { self.inner.to_ref() }.collect(None)?;
        let inner = ManuallyDrop::new(self).inner;
        Ok(unsafe { inner.into_box().into_inner() })
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &unsafe { self.inner.to_ref() }.value
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        unsafe {
            SharedInner::drop_shared(self.inner);
        }
    }
}

#[derive(Debug)]
pub(crate) struct SharedInner<T> {
    count: AtomicUsize,
    parker: NativeParker,
    value: T,
}

impl<T> SharedInner<T> {
    #[inline]
    pub fn boxed(value: T) -> BoxPtr<Self> {
        BoxPtr::alloc(Self::new(value))
    }

    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            count: AtomicUsize::new(3),
            parker: NativeParker::new(),
            value,
        }
    }

    pub unsafe fn into_inner(self) -> T {
        self.value
    }
}

impl<T> SharedInner<T> {
    const MAX_REFCOUNT: usize = (isize::MAX) as usize;

    #[inline]
    pub fn borrow_count(&self) -> usize {
        (self.count.load(Ordering::Relaxed) / 2) - 1
    }

    #[inline]
    pub unsafe fn drop_shared(slf: BoxPtr<Self>) {
        if slf.to_ref().count.fetch_sub(3, Ordering::Release) == 3 {
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
                2 => {
                    // Shared is in collection phase and this was the last reference.
                    // it will wait for us to readjust the count, so that the
                    // allocation is not dropped before we can notify it
                    inner.parker.unpark();
                    // inner may have given up waiting, in which case the update will fail
                    if inner
                        .count
                        .compare_exchange(2, 1, Ordering::Release, Ordering::Relaxed)
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

    pub fn collect(&self, timeout: Option<Instant>) -> Result<bool, LockError> {
        // be sure to synchronize with the last dropped ref
        let mut count = self.count.load(Ordering::Acquire);
        if count == 3 {
            // only the Shared remains, nothing to do
            return Ok(true);
        }
        // start collection
        let mut loops = 0;
        count = self.count.fetch_sub(1, Ordering::Relaxed) - 1;
        if count == 2 {
            // last reference was dropped between acquiring and updating the count
            // use acquire here to synchronize with that drop
            count = self.count.swap(3, Ordering::Acquire);
            assert_eq!(count, 2, "Invalid shared state");
            return Ok(true);
        }
        'outer: while count != 1 {
            if !self.parker.park(timeout)? {
                if self.count.load(Ordering::Acquire) == 2 {
                    panic!("should have unparked");
                }
                return Ok(false);
            }

            count = self.count.load(Ordering::Acquire);
            if count == 2 {
                // last reference was interrupted while notifying us.
                // rather than relying on thread::yield_now, spin for
                // a little and then require the ref to notify us again
                let mut retries = 50;
                loop {
                    if retries > 0 {
                        spin_loop_hint();
                        count = self.count.load(Ordering::Relaxed);
                        if count == 1 {
                            break 'outer;
                        }
                        retries -= 1;
                    } else {
                        match self.count.compare_exchange(
                            2,
                            4,
                            Ordering::Acquire,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => {
                                loops += 1;
                                if loops > 100 {
                                    panic!("uh oh");
                                }
                                break; // park again
                            }
                            Err(1) => break 'outer,
                            Err(_) => panic!("Invalid shared state"),
                        }
                    }
                }
            }
        }
        // end collection
        count = self.count.swap(3, Ordering::Relaxed);
        assert_eq!(count, 1, "Invalid shared state");
        return Ok(true);
    }
}

/// A tracking value which notifies its source when dropped
#[derive(Debug)]
pub struct SharedRef<T> {
    inner: BoxPtr<SharedInner<T>>,
}

unsafe impl<T: Send> Send for SharedRef<T> {}

impl<T> SharedRef<T> {
    pub(crate) fn new(inner: BoxPtr<SharedInner<T>>) -> Self {
        unsafe { SharedInner::inc_count_ref(inner.to_ptr()) };
        SharedRef { inner }
    }

    /// Get the current number of borrows, including this one
    #[inline]
    pub fn borrow_count(&self) -> usize {
        unsafe { self.inner.to_ref() }.borrow_count()
    }

    /// Create a temporary reference without increasing the borrow count
    pub const fn scoped_ref(&self) -> Ref<'_, T> {
        Ref {
            inner: self.inner,
            _marker: PhantomData,
        }
    }
}

impl<T> Clone for SharedRef<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self::new(self.inner)
    }
}

impl<T> Deref for SharedRef<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &unsafe { self.inner.to_ref() }.value
    }
}

impl<T> Drop for SharedRef<T> {
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
pub struct Ref<'s, T> {
    inner: BoxPtr<SharedInner<T>>,
    _marker: PhantomData<&'s ()>,
}

unsafe impl<T: Send> Send for Ref<'_, T> {}

impl<T> Ref<'_, T> {
    /// Get the current number of borrows, including this one
    #[inline]
    pub fn borrow_count(&self) -> usize {
        unsafe { self.inner.to_ref() }.borrow_count()
    }

    /// Construct a new `SharedRef<T>` for the associated `Shared<T>`
    #[inline]
    pub fn borrow(&self) -> SharedRef<T> {
        SharedRef::new(self.inner)
    }
}

impl<T> Clone for Ref<'_, T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner,
            _marker: PhantomData,
        }
    }
}
impl<T> Copy for Ref<'_, T> {}

impl<T> Deref for Ref<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &unsafe { self.inner.to_ref() }.value
    }
}

/// Pin a value to the stack while it is being shared
#[derive(Debug)]
pub struct PinShared<T> {
    inner: SharedInner<T>,
}

impl<T> PinShared<T> {
    /// Construct a new `PinShared<T>`
    pub fn new(data: T) -> Self {
        Self {
            inner: SharedInner::new(data),
        }
    }

    /// Evaluate a `Future` in the context of the shared value. Any borrows
    /// of the value must be returned before the future will resolve,
    /// and will block the current thread if the future is dropped after
    /// being polled
    pub async fn async_with<'s, F, Fut, R>(&'s mut self, f: F) -> R
    where
        F: FnOnce(ScopedRef<T>) -> Fut + 's,
        Fut: Future<Output = R> + 's,
    {
        let _collect = CollectOnDrop(&self.inner);
        f(ScopedRef::new(&self.inner)).await
    }

    /// Evaluate a function in the context of the shared value. This method will
    /// block until all borrows of the value are dropped, allowing them to be safely
    /// passed to other threads.
    pub fn with<'s, R>(&'s mut self, f: impl FnOnce(ScopedRef<T>) -> R) -> R {
        let _collect = CollectOnDrop(&self.inner);
        f(ScopedRef::new(&self.inner))
    }

    /// Unwrap the contained value
    #[inline]
    pub fn into_inner(self) -> T {
        unsafe { self.inner.into_inner() }
    }
}

struct CollectOnDrop<'s, T>(&'s SharedInner<T>);

impl<T> Drop for CollectOnDrop<'_, T> {
    fn drop(&mut self) {
        self.0.collect(None).unwrap();
    }
}

/// A tracking value which notifies its source when dropped
#[derive(Debug)]
pub struct ScopedRef<T> {
    inner: *const SharedInner<T>,
}

unsafe impl<T: Send> Send for ScopedRef<T> {}
unsafe impl<T: Send> Sync for ScopedRef<T> {}

impl<T> ScopedRef<T> {
    #[inline]
    pub(crate) fn new(inner: *const SharedInner<T>) -> Self {
        unsafe { SharedInner::inc_count_ref(inner) };
        ScopedRef { inner }
    }

    /// Get the current number of borrows, including this one
    #[inline]
    pub fn borrow_count(&self) -> usize {
        unsafe { &*self.inner }.borrow_count()
    }
}

impl<T> Clone for ScopedRef<T> {
    fn clone(&self) -> Self {
        Self::new(self.inner)
    }
}

impl<T> Deref for ScopedRef<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &unsafe { &*self.inner }.value
    }
}

impl<T> Drop for ScopedRef<T> {
    fn drop(&mut self) {
        unsafe { SharedInner::dec_count_ref(self.inner) };
    }
}
