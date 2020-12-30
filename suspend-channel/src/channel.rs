use core::{
    fmt::{self, Debug, Formatter},
    future::Future,
    marker::PhantomData,
    mem::ManuallyDrop,
    pin::Pin,
    sync::atomic::{fence, spin_loop_hint, AtomicU8, Ordering},
    task::{Context, Poll, Waker},
};
use std::time::Instant;

use futures_core::{FusedFuture, FusedStream, Stream};
use suspend_core::{listen::block_on_poll, Expiry};

use super::error::{Incomplete, TimedOut};
use super::util::{BoxPtr, Maybe, MaybeCopy};

const STATE_DONE: u8 = 0b0000;
const STATE_INIT: u8 = 0b0001;
const STATE_LOCKED: u8 = 0b0010;
const STATE_LOADED: u8 = 0b0100;
const STATE_WAKE: u8 = 0b1000;
const WAKE_RECV: u8 = STATE_WAKE;
const WAKE_SEND: u8 = STATE_WAKE | STATE_LOADED;

/// Create a channel for sending a single value between a producer and consumer.
pub fn send_once<T>() -> (SendOnce<T>, ReceiveOnce<T>) {
    let channel = BoxPtr::new(Box::new(Channel::new()));
    (
        SendOnce { channel },
        ReceiveOnce {
            channel: channel.into(),
        },
    )
}

/// Create a channel for sending multiple values between a producer and consumer,
/// with synchronization between each consecutive result.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let channel = BoxPtr::new(Box::new(Channel::new()));
    (
        Sender {
            channel: Some(channel),
        },
        Receiver {
            channel: Some(channel).into(),
        },
    )
}

pub(crate) struct Channel<T> {
    state: AtomicU8,
    value: Maybe<T>,
    waker: Maybe<Waker>,
}

impl<T> Channel<T> {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(STATE_INIT),
            value: Maybe::empty(),
            waker: Maybe::empty(),
        }
    }

    #[inline]
    fn is_done(&self) -> bool {
        self.state.load(Ordering::Relaxed) == STATE_DONE
    }

    /// Try to receive a value, registering a waker if none is stored.
    /// Ready value is (stored value, dropped flag).
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<(Option<T>, bool)> {
        match self.wait_for_lock() {
            STATE_DONE => {
                // sender dropped without storing a value
                self.state.store(STATE_DONE, Ordering::Relaxed);
                return Poll::Ready((None, true));
            }
            STATE_LOADED => {
                // sender dropped after storing a value
                let value = unsafe { self.value.load() };
                self.state.store(STATE_DONE, Ordering::Relaxed);
                return Poll::Ready((Some(value), true));
            }
            WAKE_SEND => {
                // sender stored a value and a waker
                let value = Some(unsafe { self.value.load() });
                let send_waker = unsafe { self.waker.load() };
                if self.state.swap(STATE_INIT, Ordering::Release) == STATE_DONE {
                    // sender dropped
                    drop(send_waker);
                    return Poll::Ready((value, true));
                }
                send_waker.wake();
                return Poll::Ready((value, false));
            }
            WAKE_RECV => {
                // drop previous recv waker
                unsafe { self.waker.clear() };
            }
            STATE_INIT => {
                // waiting for sender
            }
            _ => panic!("Invalid state"),
        }

        unsafe { self.waker.store(cx.waker().clone()) };
        if self.state.swap(WAKE_RECV, Ordering::Release) == STATE_DONE {
            // sender dropped
            unsafe { self.waker.clear() };
            return Poll::Ready((None, true));
        }
        Poll::Pending
    }

    /// Try to receive a value without registering a waker.
    /// Ready value is (stored value, dropped flag).
    pub fn try_recv(&mut self) -> Poll<(Option<T>, bool)> {
        let mut locked = false;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match state {
                STATE_INIT | WAKE_RECV => {
                    return Poll::Pending;
                }
                STATE_DONE => {
                    // sender dropped without storing a value
                    if locked {
                        self.state.store(STATE_DONE, Ordering::Relaxed);
                    }
                    return Poll::Ready((None, true));
                }
                STATE_LOADED => {
                    // sender dropped after storing a value
                    let value = unsafe { self.value.load() };
                    self.state.store(STATE_DONE, Ordering::Relaxed);
                    return Poll::Ready((Some(value), true));
                }
                WAKE_SEND => {
                    // sender stored a value and a waker
                    if !locked {
                        // need to lock the state now to avoid conflict if the sender is dropped
                        state = self.wait_for_lock();
                        locked = true;
                        continue;
                    }
                    let value = Some(unsafe { self.value.load() });
                    let send_waker = unsafe { self.waker.load() };
                    if self.state.swap(STATE_INIT, Ordering::Release) == STATE_DONE {
                        // sender dropped
                        drop(send_waker);
                        return Poll::Ready((value, true));
                    }
                    send_waker.wake();
                    return Poll::Ready((value, false));
                }
                _ => panic!("Invalid state"),
            }
        }
    }

    /// Returns (stored value, dropped flag).
    pub fn cancel_recv(&mut self) -> (Option<T>, bool) {
        match self.state.fetch_and(STATE_LOADED, Ordering::Release) {
            prev if prev & STATE_LOCKED != 0 => {
                // sender was holding the lock, will handle drop
                (None, false)
            }
            STATE_INIT => {
                // sender still active
                (None, false)
            }
            STATE_DONE => {
                // sender dropped
                (None, true)
            }
            STATE_LOADED => {
                // sender loaded a value and dropped
                (Some(unsafe { self.value.load() }), true)
            }
            WAKE_SEND => {
                // sender loaded a value, waiting for receiver
                // the state is now STATE_LOADED
                unsafe { self.waker.load() }.wake();
                (None, false)
            }
            WAKE_RECV => {
                // drop previous waker
                unsafe { self.waker.clear() };
                (None, false)
            }
            _ => panic!("Invalid state"),
        }
    }

    // Returns (stored value, dropped flag).
    pub fn cancel_recv_poll(&mut self) -> (Option<T>, bool) {
        match self.wait_for_lock() {
            prev if prev & STATE_LOCKED != 0 => {
                // sender was holding the lock, will handle drop
                (None, false)
            }
            STATE_DONE => {
                // sender dropped
                (None, true)
            }
            STATE_LOADED => {
                // sender loaded a value and dropped
                (Some(unsafe { self.value.load() }), true)
            }
            WAKE_SEND => {
                // sender loaded a value, waiting for receiver
                let value = Some(unsafe { self.value.load() });
                let send_waker = unsafe { self.waker.load() };
                self.state.store(STATE_INIT, Ordering::Release);
                send_waker.wake();
                (value, false)
            }
            WAKE_RECV => {
                // drop previous waker
                unsafe { self.waker.clear() };
                self.state.store(STATE_INIT, Ordering::Release);
                (None, false)
            }
            _ => panic!("Invalid state"),
        }
    }

    /// Error value is (unstored value, dropped flag).
    pub fn send(&mut self, value: T, cx: Option<&mut Context<'_>>) -> Result<(), (T, bool)> {
        let recv_waker = match self.wait_for_lock() {
            STATE_INIT => {
                // receiver is waiting
                None
            }
            STATE_DONE => {
                // receiver dropped
                self.state.store(STATE_DONE, Ordering::Relaxed);
                return Err((value, true));
            }
            WAKE_RECV => {
                // receiver stored a waker
                Some(unsafe { self.waker.load() })
            }
            _ => panic!("Invalid state"),
        };

        unsafe { self.value.store(value) };
        let state = if let Some(cx) = cx {
            unsafe { self.waker.store(cx.waker().clone()) };
            WAKE_SEND
        } else {
            STATE_LOADED
        };
        if self.state.swap(state, Ordering::Release) == STATE_DONE {
            // receiver dropped
            drop(recv_waker);
            if state == WAKE_SEND {
                unsafe { self.waker.clear() };
            }
            return Err((unsafe { self.value.load() }, true));
        }
        recv_waker.map(Waker::wake);
        Ok(())
    }

    fn poll_send(&mut self, cx: &mut Context<'_>) -> Poll<(Option<T>, bool)> {
        match self.wait_for_lock() {
            STATE_DONE => {
                // receiver completed and dropped
                Poll::Ready((None, true))
            }
            prev @ STATE_INIT | prev @ WAKE_RECV => {
                // receiver completed and reset
                self.state.store(prev, Ordering::Release);
                Poll::Ready((None, false))
            }
            STATE_LOADED => {
                // receiver dropped and left the result
                Poll::Ready((Some(unsafe { self.value.load() }), true))
            }
            WAKE_SEND => {
                // still waiting for receiver
                unsafe { self.waker.replace(cx.waker().clone()) };
                self.state.store(WAKE_SEND, Ordering::Release);
                Poll::Pending
            }
            _ => panic!("Invalid state"),
        }
    }

    /// Returns dropped flag.
    pub fn cancel_send(&mut self) -> bool {
        match self.state.swap(STATE_DONE, Ordering::Release) {
            prev if prev & STATE_LOCKED != 0 => {
                // receiver has the lock, will handle drop
                false
            }
            STATE_INIT => {
                // receiver still active
                false
            }
            STATE_DONE => {
                // receiver already dropped
                true
            }
            WAKE_RECV => {
                // receiver loaded a waker
                unsafe { self.waker.load() }.wake();
                false
            }
            _ => panic!("Invalid state"),
        }
    }

    fn cancel_send_poll(&mut self) -> bool {
        match self.state.fetch_or(STATE_LOCKED, Ordering::AcqRel) {
            prev if prev & STATE_LOCKED != 0 => {
                // lock held by receiver
                false
            }
            STATE_INIT => {
                // receiver completed and reset
                self.state.store(STATE_DONE, Ordering::Release);
                false
            }
            STATE_DONE => {
                // receiver completed and dropped
                true
            }
            WAKE_SEND => {
                // still waiting for receiver
                unsafe { self.waker.clear() };
                self.state.store(STATE_LOADED, Ordering::Release);
                false
            }
            _ => panic!("Invalid state"),
        }
    }

    pub fn wait_recv(&mut self) -> (Option<T>, bool) {
        if let Poll::Ready(result) = self.wait_recv_poll(None) {
            result
        } else {
            unreachable!()
        }
    }

    pub fn wait_recv_timeout(&mut self, timeout: Option<Instant>) -> (Option<T>, bool) {
        match self.wait_recv_poll(timeout) {
            Poll::Ready(result) => result,
            Poll::Pending => self.cancel_recv_poll(),
        }
    }

    #[inline]
    fn wait_for_lock(&mut self) -> u8 {
        loop {
            let prev = self.state.fetch_or(STATE_LOCKED, Ordering::Relaxed);
            if prev & STATE_LOCKED == 0 {
                fence(Ordering::Acquire);
                return prev;
            }
            spin_loop_hint();
        }
    }

    #[inline]
    fn wait_recv_poll(&mut self, timeout: impl Expiry) -> Poll<(Option<T>, bool)> {
        let mut first = true;
        let timeout: Option<Instant> = timeout.into_expire();
        block_on_poll(
            |cx| {
                if first {
                    first = false;
                    self.poll_recv(cx)
                } else {
                    // no need to update the waker after the first poll
                    self.try_recv()
                }
            },
            timeout,
        )
    }
}

impl<T> Debug for Channel<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("done", &self.is_done())
            .finish()
    }
}

/// A [`Future`] which resolves once a send has completed.
#[derive(Debug)]
pub struct TrackSend<'a, T> {
    channel: MaybeCopy<Option<BoxPtr<Channel<T>>>>,
    value: Maybe<Option<T>>,
    drops: bool,
    _marker: PhantomData<&'a mut T>,
}

impl<T> Drop for TrackSend<'_, T> {
    fn drop(&mut self) {
        unsafe {
            let channel = self.channel.load();
            channel.map(|mut c| c.cancel_send_poll());
            self.value.clear();
        }
    }
}

impl<T> Future for TrackSend<'_, T> {
    type Output = Result<(), T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(value) = unsafe { self.value.replace(None) } {
            if let Some(mut channel) = unsafe { self.channel.load() } {
                if let Err((value, dropped)) = channel.send(value, Some(cx)) {
                    if dropped && self.drops {
                        drop(channel.into_box());
                    }
                    unsafe { self.channel.store(None) };
                    Poll::Ready(Err(value))
                } else {
                    Poll::Pending
                }
            } else {
                Poll::Ready(Err(value))
            }
        } else if let Some(mut channel) = unsafe { self.channel.load() } {
            channel.poll_send(cx).map(|(result, dropped)| {
                if dropped && self.drops {
                    drop(channel.into_box());
                }
                unsafe { self.channel.store(None) };
                result.map(Err).unwrap_or(Ok(()))
            })
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

impl<T> FusedFuture for TrackSend<'_, T> {
    fn is_terminated(&self) -> bool {
        unsafe { self.channel.as_ref() }.is_none()
    }
}

/// Created by [`send_once()`] and used to dispatch a single value
/// to an associated [`ReceiveOnce`] instance.
#[derive(Debug)]
pub struct SendOnce<T> {
    channel: BoxPtr<Channel<T>>,
}

unsafe impl<T: Send> Send for SendOnce<T> {}
unsafe impl<T: Send> Sync for SendOnce<T> {}

impl<T> SendOnce<T> {
    /// Check if the receiver has already been dropped.
    pub fn is_canceled(&self) -> bool {
        self.channel.is_done()
    }

    /// Dispatch the result and consume the [`SendOnce`].
    pub fn send(self, value: T) -> Result<(), T> {
        let mut channel = ManuallyDrop::new(self).channel;
        channel.send(value, None).map_err(|(value, drop_channel)| {
            if drop_channel {
                drop(channel.into_box());
            }
            value
        })
    }

    /// Load a value to be sent, returning a [`Future`] which resolves when
    /// the value is received or the [`ReceiveOnce`] is dropped.
    pub fn track_send(self, value: T) -> TrackSend<'static, T> {
        let channel = ManuallyDrop::new(self).channel;
        TrackSend {
            channel: Some(channel).into(),
            value: Some(value).into(),
            drops: true,
            _marker: PhantomData,
        }
    }
}

impl<T> Drop for SendOnce<T> {
    fn drop(&mut self) {
        if self.channel.cancel_send() {
            drop(self.channel.into_box());
        }
    }
}

/// Created by [`send_once()`] and used to receive a single value from
/// an associated [`SendOnce`] instance.
#[derive(Debug)]
pub struct ReceiveOnce<T> {
    channel: MaybeCopy<BoxPtr<Channel<T>>>,
}

unsafe impl<T: Send> Send for ReceiveOnce<T> {}
unsafe impl<T: Send> Sync for ReceiveOnce<T> {}

impl<T> ReceiveOnce<T> {
    /// Safely cancel the receive operation, consuming the [`ReceiveOnce`].
    pub fn cancel(self) -> Option<T> {
        let mut channel = unsafe { ManuallyDrop::new(self).channel.load() };
        let (result, dropped) = channel.cancel_recv();
        if dropped {
            drop(channel.into_box());
        }
        result
    }

    /// Try to receive the value, consuming the [`ReceiveOnce`] if the value
    /// has been loaded or the [`SendOnce`] has been dropped.
    pub fn try_recv(self) -> Result<Result<T, Incomplete>, Self> {
        let mut channel = unsafe { self.channel.load() };
        match channel.try_recv() {
            Poll::Ready((result, dropped)) => {
                let _ = ManuallyDrop::new(self);
                if dropped {
                    drop(channel.into_box());
                }
                Ok(result.ok_or(Incomplete))
            }
            Poll::Pending => {
                unsafe { self.channel.store(channel) };
                Err(self)
            }
        }
    }

    /// Block the current thread until a value is received or the [`SendOnce`] is dropped.
    pub fn wait(self) -> Result<T, Incomplete> {
        let mut channel = unsafe { ManuallyDrop::new(self).channel.load() };
        let (result, dropped) = channel.wait_recv();
        if dropped {
            drop(channel.into_box());
        }
        result.ok_or(Incomplete)
    }

    /// Block the current thread until a value is received or the [`SendOnce`] is dropped,
    /// returning `Err(Self)` if a timeout is reached.
    pub fn wait_timeout(self, timeout: impl Expiry) -> Result<Result<T, Incomplete>, Self> {
        let mut channel = unsafe { self.channel.load() };
        match channel.wait_recv_timeout(timeout.into_expire()) {
            (Some(result), true) => {
                let _ = ManuallyDrop::new(self);
                drop(channel.into_box());
                Ok(Ok(result))
            }
            (None, true) => {
                let _ = ManuallyDrop::new(self);
                drop(channel.into_box());
                Ok(Err(Incomplete))
            }
            (Some(result), false) => {
                let _ = ManuallyDrop::new(self);
                Ok(Ok(result))
            }
            (None, false) => Err(self),
        }
    }
}

impl<T> Drop for ReceiveOnce<T> {
    fn drop(&mut self) {
        let mut channel = unsafe { self.channel.load() };
        if let (_, true) = channel.cancel_recv() {
            drop(channel.into_box());
        }
    }
}

impl<T> Future for ReceiveOnce<T> {
    type Output = Result<T, Incomplete>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut channel = unsafe { self.channel.load() };
        channel.poll_recv(cx).map(|r| r.0.ok_or(Incomplete))
    }
}

impl<T> FusedFuture for ReceiveOnce<T> {
    fn is_terminated(&self) -> bool {
        unsafe { self.channel.as_ref() }.is_done()
    }
}

impl<T> Stream for ReceiveOnce<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut channel = unsafe { self.channel.load() };
        channel.poll_recv(cx).map(|r| r.0)
    }
}

impl<T> FusedStream for ReceiveOnce<T> {
    fn is_terminated(&self) -> bool {
        unsafe { self.channel.as_ref() }.is_done()
    }
}

/// Created by [`channel()`] and used to dispatch a stream of values to a
/// an associated [`Receiver`].
#[derive(Debug)]
pub struct Sender<T> {
    channel: Option<BoxPtr<Channel<T>>>,
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}

impl<T> Sender<T> {
    /// Check if the receiver has already been dropped.
    pub fn is_canceled(&self) -> bool {
        self.channel.map(|c| c.is_done()).unwrap_or(true)
    }

    /// Dispatch the result and consume the [`Sender`].
    pub fn send(&mut self, value: T) -> TrackSend<'_, T> {
        TrackSend {
            channel: self.channel.into(),
            value: Some(value).into(),
            drops: false,
            _marker: PhantomData,
        }
    }

    /// Send a single result and consume the [`Sender`].
    pub fn into_send(self, value: T) -> Result<(), T> {
        if let Some(mut channel) = ManuallyDrop::new(self).channel {
            channel.send(value, None).map_err(|(result, drop_channel)| {
                if drop_channel {
                    drop(channel.into_box());
                }
                result
            })
        } else {
            Err(value)
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if let Some(mut channel) = self.channel.take() {
            if channel.cancel_send() {
                drop(channel.into_box());
            }
        }
    }
}

/// Created by [`channel()`] and used to receive a stream of values from
/// an associated [`Sender`].
#[derive(Debug)]
pub struct Receiver<T> {
    channel: MaybeCopy<Option<BoxPtr<Channel<T>>>>,
}

unsafe impl<T: Send> Send for Receiver<T> {}
unsafe impl<T: Send> Sync for Receiver<T> {}

impl<T> Receiver<T> {
    /// Safely cancel the receive operation, consuming the [`Receiver`].
    pub fn cancel(self) -> Option<T> {
        if let Some(mut channel) = unsafe { ManuallyDrop::new(self).channel.load() } {
            let (result, dropped) = channel.cancel_recv();
            if dropped {
                drop(channel.into_box());
            }
            result
        } else {
            None
        }
    }

    /// Try to receive the next value from the stream.
    pub fn try_recv(&mut self) -> Poll<Option<T>> {
        if let Some(mut channel) = unsafe { self.channel.load() } {
            channel.try_recv().map(|(result, dropped)| {
                if dropped || result.is_some() {
                    if dropped {
                        drop(channel.into_box());
                        unsafe { self.channel.store(None) };
                    }
                }
                result
            })
        } else {
            Poll::Ready(None)
        }
    }

    /// Block the current thread on the next result from the [`Sender`], returning
    /// [`None`] if the sender has been dropped.
    pub fn wait_next(&mut self) -> Option<T> {
        if let Some(mut channel) = unsafe { self.channel.load() } {
            let (result, dropped) = channel.wait_recv();
            if dropped {
                drop(channel.into_box());
                unsafe { self.channel.replace(None) };
            }
            result
        } else {
            None
        }
    }

    /// Block the current thread on the next result from the [`Sender`], returning
    /// `Ok(None)`] if the sender has been dropped and `Err(Incomplete)` if the
    /// provided timeout is reached.
    pub fn wait_next_timeout(&mut self, timeout: impl Expiry) -> Result<Option<T>, TimedOut> {
        if let Some(mut channel) = unsafe { self.channel.load() } {
            let (result, dropped) = channel.wait_recv_timeout(timeout.into_expire());
            if dropped {
                drop(channel.into_box());
                unsafe { self.channel.replace(None) };
                Ok(result)
            } else if result.is_none() {
                Err(TimedOut)
            } else {
                Ok(result)
            }
        } else {
            Ok(None)
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if let Some(mut channel) = unsafe { self.channel.load() } {
            if channel.cancel_recv().1 {
                drop(channel.into_box());
            }
        }
    }
}

impl<T> Stream for Receiver<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        if let Some(mut channel) = unsafe { self.channel.load() } {
            channel.poll_recv(cx).map(|(result, dropped)| {
                if dropped {
                    drop(channel.into_box());
                    unsafe { self.channel.store(None) };
                }
                result
            })
        } else {
            Poll::Ready(None)
        }
    }
}

impl<T> FusedStream for Receiver<T> {
    fn is_terminated(&self) -> bool {
        unsafe { self.channel.load() }
            .map(|c| c.is_done())
            .unwrap_or(true)
    }
}
