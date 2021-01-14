//! Macros for defining a custom Stream using an async function.

use core::{
    cell::RefCell,
    fmt::{self, Debug, Formatter},
    future::Future,
    marker::PhantomData,
    mem,
    pin::Pin,
    ptr,
    task::{Context, Poll},
};

use futures_core::{FusedStream, Stream};

use crate::util::StreamIter;

thread_local! {
    static CHANNEL: RefCell<*mut ()> = RefCell::new(ptr::null_mut());
}

#[inline]
fn stream_poll<T, R>(poll: impl FnOnce() -> R) -> (Option<T>, R) {
    CHANNEL.with(|channel| {
        let mut item = None;
        let prev_ptr = channel.replace(&mut item as *mut Option<T> as *mut ());
        let result = poll();
        channel.replace(prev_ptr);
        (item, result)
    })
}

#[inline]
fn stream_send<T>(value: T) {
    CHANNEL.with(|channel| {
        let opt_ptr = channel.replace(ptr::null_mut()) as *mut Option<T>;
        if opt_ptr.is_null() || unsafe { &mut *opt_ptr }.replace(value).is_some() {
            panic!(
                "Invalid async_stream usage: {}",
                if opt_ptr.is_null() {
                    "no active channel"
                } else {
                    "channel filled"
                }
            );
        }
    })
}

#[inline]
fn stream_flush<T>() -> bool {
    CHANNEL.with(|channel| !(&*channel.borrow()).is_null())
}

/// A utility class for providing values to the stream.
#[derive(Debug)]
pub struct AsyncStreamScope<'a, T> {
    _marker: PhantomData<&'a mut std::cell::Cell<T>>,
}

impl<'a, T> AsyncStreamScope<'a, T> {
    pub(crate) fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }

    /// Dispatch a value to the stream, returning a [`Future`] which will resolve
    /// when the value has been received.
    pub fn send<'b>(&'b mut self, value: T) -> AsyncStreamFlush<'b, T>
    where
        'a: 'b,
    {
        stream_send(value);
        AsyncStreamFlush {
            first: true,
            _marker: PhantomData,
        }
    }
}

impl<T> Clone for AsyncStreamScope<'_, T> {
    fn clone(&self) -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

/// A [`Future`] which resolves when the dispatched value has been received.
#[derive(Debug)]
pub struct AsyncStreamFlush<'a, T> {
    first: bool,
    _marker: PhantomData<&'a mut std::cell::Cell<T>>,
}

impl<T> Future for AsyncStreamFlush<'_, T> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.first || !stream_flush::<T>() {
            // wait on the first poll - the channel has just been filled.
            // on a subsequent poll the channel pointer must have been replaced.
            self.first = false;
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

/// A [`Stream`] implementation wrapping a generator `Future`.
#[derive(Debug)]
pub struct AsyncStream<'a, I: AsyncStreamInit<'a>> {
    state: AsyncStreamState<'a, I>,
    _marker: PhantomData<&'a mut std::cell::Cell<I>>,
}

enum AsyncStreamState<'a, I: AsyncStreamInit<'a>> {
    Init(I),
    Poll(I::Fut),
    Complete,
}

impl<'a, I: AsyncStreamInit<'a>> Debug for AsyncStreamState<'a, I> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AsyncStreamState")
            .field(&match self {
                Self::Init(..) => "init",
                Self::Poll(..) => "poll",
                Self::Complete => "complete",
            })
            .finish()
    }
}

/// Construct a new [`AsyncStream`] from a generator function.
pub fn make_stream<'a, T, I, F>(init: I) -> AsyncStream<'a, AsyncStreamFn<I, T, F>>
where
    I: FnOnce(AsyncStreamScope<'a, T>) -> F + 'a,
    F: Future<Output = ()> + 'a,
{
    AsyncStreamFn(init, PhantomData).into()
}

/// Trait for async stream initializers
pub trait AsyncStreamInit<'a> {
    /// The item type of the Stream
    type Item;
    /// The type of the `Future` that produces the stream results
    type Fut: Future<Output = ()>;

    /// Run the stream constructor
    fn into_future(self, scope: AsyncStreamScope<'a, Self::Item>) -> Self::Fut;
}

/// A constructor for an async stream.
#[derive(Debug)]
pub struct AsyncStreamFn<I, T, F>(I, PhantomData<(T, F)>);

impl<'a, T, I, F> AsyncStreamInit<'a> for AsyncStreamFn<I, T, F>
where
    I: FnOnce(AsyncStreamScope<'a, T>) -> F,
    F: Future<Output = ()>,
    T: 'a,
{
    type Item = T;
    type Fut = F;

    fn into_future(self, scope: AsyncStreamScope<'a, Self::Item>) -> Self::Fut {
        (self.0)(scope)
    }
}

#[doc(hidden)]
pub fn try_stream_fn<'a, T, E, I, F>(init: I) -> impl AsyncStreamInit<'a, Item = Result<T, E>>
where
    I: FnOnce(AsyncStreamScope<'a, Result<T, E>>) -> F,
    F: Future<Output = Result<(), E>>,
    T: 'a,
    E: 'a,
{
    AsyncStreamFn(
        |sender| async move {
            if let Err(err) = init(sender).await {
                stream_send(Result::<T, E>::Err(err));
            }
        },
        PhantomData,
    )
}

impl<'a, I> Stream for AsyncStream<'a, I>
where
    I: AsyncStreamInit<'a>,
{
    type Item = I::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let slf = unsafe { Pin::get_unchecked_mut(self) };
        loop {
            if let AsyncStreamState::Poll(stream) = &mut slf.state {
                let (optval, result) =
                    stream_poll(move || unsafe { Pin::new_unchecked(stream) }.poll(cx));
                if let Poll::Ready(()) = result {
                    slf.state = AsyncStreamState::Complete;
                    break Poll::Ready(optval);
                } else {
                    break if optval.is_some() {
                        Poll::Ready(optval)
                    } else {
                        Poll::Pending
                    };
                }
            }
            match mem::replace(&mut slf.state, AsyncStreamState::Complete) {
                AsyncStreamState::Init(init) => {
                    let fut = init.into_future(AsyncStreamScope::new());
                    slf.state = AsyncStreamState::Poll(fut);
                }
                AsyncStreamState::Complete => {
                    slf.state = AsyncStreamState::Complete;
                    break Poll::Ready(None);
                }
                _ => unreachable!(),
            }
        }
    }
}

impl<'a, I> From<I> for AsyncStream<'a, I>
where
    I: AsyncStreamInit<'a>,
{
    fn from(init: I) -> Self {
        AsyncStream {
            state: AsyncStreamState::Init(init),
            _marker: PhantomData,
        }
    }
}

impl<'a, I> FusedStream for AsyncStream<'a, I>
where
    I: AsyncStreamInit<'a>,
{
    fn is_terminated(&self) -> bool {
        matches!(self.state, AsyncStreamState::Complete)
    }
}

impl<'a, I> IntoIterator for Pin<&mut AsyncStream<'a, I>>
where
    I: AsyncStreamInit<'a>,
{
    type IntoIter = StreamIter<Self>;
    type Item = I::Item;

    fn into_iter(self) -> Self::IntoIter {
        StreamIter::from(self)
    }
}

/// A macro for constructing an async stream from an async generator function.
#[macro_export]
macro_rules! stream {
    {$($block:tt)*} => {
        $crate::async_stream::make_stream(move |mut __sender| async move {
            #[allow(unused)]
            macro_rules! send {
                ($v:expr) => {
                    __sender.send($v).await;
                }
            }
            $($block)*
        })
    }
}

/// A macro for constructing an async stream of `Result<T, E>` from an async
/// generator function.
#[macro_export]
macro_rules! try_stream {
    {$($block:tt)*} => {
        $crate::async_stream::AsyncStream::from($crate::async_stream::try_stream_fn(move |mut __sender| async move {
            #[allow(unused)]
            macro_rules! send {
                ($v:expr) => {
                    __sender.send(Ok($v)).await;
                }
            }
            $($block)*
        }))
    }
}
