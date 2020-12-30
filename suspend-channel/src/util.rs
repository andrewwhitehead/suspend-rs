use core::{
    cell::Cell,
    fmt::{self, Debug, Formatter},
    future::Future,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::NonNull,
    task::{Context, Poll},
};

use futures_core::{FusedFuture, FusedStream, Stream};

#[cfg(debug_assertions)]
pub use maybe_cell::checked::{Maybe, MaybeCopy};
#[cfg(not(debug_assertions))]
pub use maybe_cell::unchecked::{Maybe, MaybeCopy};

#[repr(transparent)]
pub struct BoxPtr<T: ?Sized>(NonNull<T>, PhantomData<Cell<T>>);

impl<T: ?Sized> BoxPtr<T> {
    #[inline]
    pub fn new(value: Box<T>) -> Self {
        unsafe { Self::new_unchecked(Box::into_raw(value)) }
        // unstable: Self(Box::into_raw_non_null(value))
    }

    #[inline]
    pub unsafe fn new_unchecked(ptr: *mut T) -> Self {
        debug_assert!(!ptr.is_null());
        Self(NonNull::new_unchecked(ptr), PhantomData)
    }

    // #[inline]
    // pub fn as_ptr(&self) -> *mut T {
    //     self.0.as_ptr()
    // }

    pub fn into_box(self) -> Box<T> {
        unsafe { Box::from_raw(self.0.as_ptr()) }
    }
}

impl<T> Clone for BoxPtr<T> {
    fn clone(&self) -> Self {
        Self(self.0, PhantomData)
    }
}

impl<T> Copy for BoxPtr<T> {}

impl<T> Deref for BoxPtr<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl<T> DerefMut for BoxPtr<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl<T: Debug> Debug for BoxPtr<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("BoxPtr").field(&self.0).finish()
    }
}

/// Provide a utility method to await the next item from a stream.
pub trait StreamNext: Stream + Unpin {
    /// Create a [`Future`] which resolves to the next item of the stream,
    /// or [`None`] if the stream has terminated.
    fn next(&mut self) -> NextFuture<'_, Self> {
        NextFuture(Some(self))
    }
}

impl<S: Stream + Unpin + ?Sized> StreamNext for S {}

/// A [`Future`] which resolves to the next item of a [`Stream`].
#[derive(Debug)]
pub struct NextFuture<'n, S: ?Sized>(Option<&'n mut S>);

impl<S: Unpin + ?Sized> Unpin for NextFuture<'_, S> {}

impl<S: Stream + Unpin + ?Sized> Future for NextFuture<'_, S> {
    type Output = Option<S::Item>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let slf = Pin::get_mut(self);
        if let Some(stream) = slf.0.as_mut() {
            let result = Pin::new(stream).poll_next(cx);
            if result.is_ready() {
                slf.0.take();
            }
            result
        } else {
            Poll::Ready(None)
        }
    }
}

impl<S> FusedFuture for NextFuture<'_, S>
where
    S: Stream + FusedStream + Unpin,
{
    fn is_terminated(&self) -> bool {
        if let Some(slf) = self.0.as_ref() {
            slf.is_terminated()
        } else {
            true
        }
    }
}
