use core::{
    fmt::Debug,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::{FusedFuture, Stream};

#[cfg(debug_assertions)]
pub use maybe_cell::checked::{Maybe, MaybeCopy};
#[cfg(not(debug_assertions))]
pub use maybe_cell::unchecked::{Maybe, MaybeCopy};

use suspend_core::listen::block_on_poll;

/// Provide a utility methods for iterating streams.
pub trait StreamIterExt: Stream + Unpin {
    /// Create a [`Future`] which resolves to the next item of the stream,
    /// or [`None`] if the stream has terminated.
    fn stream_into_iter(self) -> StreamIter<Self>
    where
        Self: Sized,
    {
        StreamIter::from(self)
    }
    /// Create a [`Future`] which resolves to the next item of the stream,
    /// or [`None`] if the stream has terminated.
    fn stream_iter(&mut self) -> StreamIterMut<'_, Self> {
        StreamIterMut(Some(self))
    }

    /// Create a [`Future`] which resolves to the next item of the stream,
    /// or [`None`] if the stream has terminated.
    fn stream_next(&mut self) -> StreamNextMut<'_, Self> {
        StreamNextMut(Some(self))
    }
}

impl<S: ?Sized + Stream + Unpin> StreamIterExt for S {}

/// A [`Future`] which resolves to the next item of a [`Stream`].
#[derive(Debug)]
pub struct StreamNextMut<'s, S: ?Sized>(Option<&'s mut S>);

impl<S: ?Sized + Unpin> Unpin for StreamNextMut<'_, S> {}

impl<S: ?Sized + Stream + Unpin> Future for StreamNextMut<'_, S> {
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

impl<S> FusedFuture for StreamNextMut<'_, S>
where
    S: Stream + Unpin,
{
    fn is_terminated(&self) -> bool {
        self.0.is_none()
    }
}

/// An [`Iterator`] which blocks on each subsequent element of a stream.
#[derive(Debug)]
pub struct StreamIter<S>(Option<S>);

impl<S, T> Iterator for StreamIter<S>
where
    S: Stream<Item = T> + Unpin,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(stream) = self.0.as_mut() {
            let mut s = Pin::new(stream);
            match block_on_poll(move |cx| s.as_mut().poll_next(cx), None) {
                Poll::Ready(result) => {
                    if result.is_none() {
                        self.0.take();
                    }
                    result
                }
                _ => unreachable!(),
            }
        } else {
            None
        }
    }
}

impl<S, T> From<S> for StreamIter<S>
where
    S: Stream<Item = T> + Unpin,
{
    #[inline]
    fn from(stream: S) -> Self {
        Self(Some(stream))
    }
}

/// An [`Iterator`] which blocks on each subsequent element of a stream.
#[derive(Debug)]
pub struct StreamIterMut<'s, S: ?Sized>(Option<&'s mut S>);

impl<S, T> Iterator for StreamIterMut<'_, S>
where
    S: Stream<Item = T> + Unpin,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(stream) = self.0.as_mut() {
            let mut s = Pin::new(stream);
            match block_on_poll(move |cx| s.as_mut().poll_next(cx), None) {
                Poll::Ready(result) => {
                    if result.is_none() {
                        self.0.take();
                    }
                    result
                }
                _ => unreachable!(),
            }
        } else {
            None
        }
    }
}
