//! Provide an equivalent of `Lender<T>` that is pinned to the stack
//! and can be used to track task completion.

use core::ops::Deref;

use crate::shared::SharedInner;
use crate::thread::block_on_poll;

/// Run a function and then block the current thread until all trackers
/// produced by the function have been dropped. This can be used to
/// track the completion across threads.
pub fn with_scope<F, R>(f: F) -> R
where
    F: FnOnce(Scoped<()>) -> R,
{
    let mut scope = PinShared::new(());
    scope.with(f)
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

    /// Evaluate a function in the context of the shared value. This method will
    /// block until all borrows of the value are dropped, allowing them to be safely
    /// passed to other threads.
    pub fn with<'s, R>(&'s mut self, f: impl FnOnce(Scoped<T>) -> R) -> R {
        let _collect = CollectOnDrop(&self.inner);
        f(Scoped::new(&self.inner))
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
        if !self.0.try_collect(false) {
            let mut first = true;
            let _ = block_on_poll(
                |cx| {
                    if first {
                        first = false;
                        self.0.start_poll(cx.waker())
                    } else {
                        self.0.resume_poll(cx.waker())
                    }
                },
                None,
            );
        }
    }
}

/// A tracking value which notifies its source when dropped
#[derive(Debug)]
pub struct Scoped<T> {
    inner: *const SharedInner<T>,
}

unsafe impl<T: Send> Send for Scoped<T> {}
unsafe impl<T: Sync> Sync for Scoped<T> {}

impl<T> Scoped<T> {
    #[inline]
    pub(crate) fn new(inner: &SharedInner<T>) -> Self {
        unsafe { SharedInner::inc_count_ref(inner) };
        Scoped { inner }
    }

    /// Get the current number of borrows, including this one
    #[inline]
    pub fn borrow_count(&self) -> usize {
        unsafe { &*self.inner }.borrow_count()
    }
}

#[cfg(feature = "std")]
impl<T> Clone for Scoped<T> {
    fn clone(&self) -> Self {
        let inner = self.inner;
        unsafe { SharedInner::inc_count_ref(inner) };
        Self { inner }
    }
}

impl<T> Deref for Scoped<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &unsafe { &*self.inner }.as_ref()
    }
}

impl<T> Drop for Scoped<T> {
    fn drop(&mut self) {
        unsafe { SharedInner::dec_count_ref(self.inner) };
    }
}
