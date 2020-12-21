// This currently does not fly in miri for the same reason generators have issues:
// https://github.com/rust-lang/unsafe-code-guidelines/issues/148
// Each poll of the AsyncStream future creates a Pin and invalidates the
// channel pointer contained in the associated AsyncStreamScope.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::ptr::NonNull;
use std::task::{Context, Poll};

use futures_core::{FusedStream, Stream};

use crate::util::Maybe;

#[inline]
unsafe fn channel_send<T>(channel: NonNull<Maybe<Option<T>>>, value: T) {
    if channel.as_ref().replace(Some(value)).is_some() {
        panic!("Invalid use of stream sender");
    }
}

#[inline]
unsafe fn channel_recv<T>(channel: &Maybe<Option<T>>) -> Option<T> {
    channel.replace(None)
}

pub struct AsyncStreamScope<'a, T> {
    channel: NonNull<Maybe<Option<T>>>,
    _marker: PhantomData<&'a mut std::cell::Cell<T>>,
}

impl<T> AsyncStreamScope<'_, T> {
    pub(crate) unsafe fn new(channel: &mut Maybe<Option<T>>) -> Self {
        Self {
            channel: NonNull::new_unchecked(channel),
            _marker: PhantomData,
        }
    }

    pub fn send<'a, 'b>(&'b mut self, value: T) -> AsyncStreamSend<'a, T>
    where
        'b: 'a,
    {
        unsafe {
            channel_send(self.channel, value);
            AsyncStreamSend {
                channel: self.channel.as_ref(),
                first: true,
                _marker: PhantomData,
            }
        }
    }
}

impl<T> Clone for AsyncStreamScope<'_, T> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel,
            _marker: PhantomData,
        }
    }
}

pub struct AsyncStreamSend<'a, T> {
    channel: &'a Maybe<Option<T>>,
    first: bool,
    _marker: PhantomData<&'a mut std::cell::Cell<T>>,
}

impl<T> Future for AsyncStreamSend<'_, T> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.first || unsafe { self.channel.as_ref().as_ref().is_some() } {
            // always wait on first poll - the sender has just been filled.
            // remain pending while the lock is occupied
            self.first = false;
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

pub struct AsyncStream<'a, T, I, F> {
    state: Maybe<AsyncStreamState<I, F>>,
    channel: Maybe<Option<T>>,
    _marker: PhantomData<&'a mut std::cell::Cell<T>>,
}

enum AsyncStreamState<I, F> {
    Init(I),
    Poll(F),
    Complete,
}

pub fn make_stream<'a, T, I, F>(init: I) -> AsyncStream<'a, T, I, F>
where
    I: FnOnce(AsyncStreamScope<'a, T>) -> F + 'a,
    F: Future<Output = ()> + 'a,
{
    AsyncStream {
        state: AsyncStreamState::Init(init).into(),
        channel: None.into(),
        _marker: PhantomData,
    }
}

impl<'a, T, I, F> Stream for AsyncStream<'a, T, I, F>
where
    I: FnOnce(AsyncStreamScope<'a, T>) -> F,
    F: Future<Output = ()>,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        unsafe {
            let slf = Pin::get_unchecked_mut(self);
            loop {
                match slf.state.as_ref() {
                    AsyncStreamState::Init(_) => {
                        let init = match slf.state.load() {
                            AsyncStreamState::Init(init) => init,
                            _ => unreachable!(),
                        };
                        let fut = init(AsyncStreamScope::new(&mut slf.channel));
                        slf.state.store(AsyncStreamState::Poll(fut));
                    }
                    AsyncStreamState::Poll(_) => {
                        let poll = match slf.state.as_mut() {
                            AsyncStreamState::Poll(poll) => Pin::new_unchecked(poll),
                            _ => unreachable!(),
                        };
                        if let Poll::Ready(_) = poll.poll(cx) {
                            slf.state.replace(AsyncStreamState::Complete);
                        } else {
                            break if let Some(val) = channel_recv(&slf.channel) {
                                Poll::Ready(Some(val))
                            } else {
                                Poll::Pending
                            };
                        }
                    }
                    AsyncStreamState::Complete => {
                        break Poll::Ready(channel_recv(&slf.channel));
                    }
                }
            }
        }
    }
}

impl<'a, T, I, F> Drop for AsyncStream<'a, T, I, F> {
    fn drop(&mut self) {
        unsafe {
            self.channel.clear();
            self.state.clear()
        };
    }
}

impl<'a, T, I, F> FusedStream for AsyncStream<'a, T, I, F>
where
    I: FnOnce(AsyncStreamScope<'a, T>) -> F,
    F: Future<Output = ()>,
{
    fn is_terminated(&self) -> bool {
        matches!(unsafe { self.state.as_ref() }, AsyncStreamState::Complete)
    }
}

pub struct TryAsyncStreamSend<'a, T, E, F> {
    channel: NonNull<Maybe<Option<Result<T, E>>>>,
    fut: F,
    _marker: PhantomData<&'a mut std::cell::Cell<T>>,
}

impl<'a, T, E, F> TryAsyncStreamSend<'a, T, E, F> {
    pub fn new(sender: AsyncStreamScope<'a, Result<T, E>>, fut: F) -> Self {
        Self {
            channel: sender.channel,
            fut,
            _marker: PhantomData,
        }
    }
}

impl<'a, T, E, F> Future for TryAsyncStreamSend<'a, T, E, F>
where
    F: Future<Output = Result<(), E>>,
{
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let channel = self.channel;
            let fut = self.map_unchecked_mut(|s| &mut s.fut);
            fut.poll(cx).map(|result| {
                if let Err(err) = result {
                    channel_send(channel, Err(err));
                }
            })
        }
    }
}

#[macro_export]
macro_rules! stream {
    {$($block:tt)*} => {
        $crate::make_stream(move |mut __sender| async move {
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

#[macro_export]
macro_rules! try_stream {
    {$($block:tt)*} => {
        $crate::make_stream(move |mut __sender| {
            $crate::TryAsyncStreamSend::new(__sender.clone(), async move {
                    macro_rules! send {
                        ($v:expr) => {
                            __sender.send(Ok($v)).await;
                        }
                    }
                    $($block)*
                }
            )
        })
    }
}
