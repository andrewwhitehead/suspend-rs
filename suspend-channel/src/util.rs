use core::{
    fmt::Debug,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::{FusedFuture, FusedStream, Stream};

#[cfg(debug_assertions)]
pub use maybe_cell::checked::{Maybe, MaybeCopy};
#[cfg(not(debug_assertions))]
pub use maybe_cell::unchecked::{Maybe, MaybeCopy};

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
