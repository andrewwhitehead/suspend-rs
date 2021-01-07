use core::{
    fmt::{self, Debug, Formatter},
    future::Future,
    marker::PhantomData,
    mem::ManuallyDrop,
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
    task::{Context, Poll, Waker},
};
use std::panic;
use std::time::Instant;

use futures_core::{FusedFuture, FusedStream, Stream};
use suspend_core::{listen::block_on_poll, util::BoxPtr, Expiry};

use super::error::RecvError;
use super::util::Maybe;

const STATE_DONE: u8 = 0b00000;
const STATE_ACTIVE: u8 = 0b00001;
const STATE_LOADED: u8 = 0b00010;
const STATE_WLOCK: u8 = 0b00100;
const STATE_RLOCK: u8 = 0b01000;
const STATE_WAKE: u8 = 0b10000;

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
            state: AtomicU8::new(STATE_ACTIVE),
            value: Maybe::empty(),
            waker: Maybe::empty(),
        }
    }

    #[inline]
    fn is_done(&self) -> bool {
        self.state.load(Ordering::Relaxed) & (STATE_ACTIVE | STATE_LOADED) == 0
    }

    pub fn write(&self, value: T, mut waker: Option<&Waker>, finalize: bool) -> Result<(), T> {
        let state = self.state.load(Ordering::Relaxed);
        if state & STATE_ACTIVE == 0 {
            // synchronize with the reader's drop
            self.state.load(Ordering::Acquire);
            return Err(value);
        }
        if state & STATE_LOADED != 0 {
            panic!("Invalid send to loaded channel");
        }
        unsafe { self.value.store(value) };
        // release - sync update to the store, above. acquire - sync update to the waker, below.
        let mut prev = self
            .state
            .fetch_or(STATE_LOADED | STATE_WLOCK | STATE_WAKE, Ordering::AcqRel);
        if prev & STATE_WLOCK == 0 {
            let recv_waker = if prev & STATE_WAKE != 0 {
                Some(unsafe { self.waker.load() })
            } else {
                None
            };
            if let Some(waker) = waker.take() {
                unsafe { self.waker.store(waker.clone()) };
                prev = self.state.fetch_and(!STATE_WLOCK, Ordering::Release);
                if prev & STATE_ACTIVE == 0 {
                    // drop the waker we just stored
                    unsafe { self.waker.clear() };
                    return Err(unsafe { self.value.load() });
                }
            } else {
                let mut remove = STATE_WLOCK | STATE_WAKE;
                if finalize {
                    remove |= STATE_ACTIVE;
                }
                prev = self.state.fetch_and(!remove, Ordering::Release);
                if prev & STATE_ACTIVE == 0 {
                    return Err(unsafe { self.value.load() });
                }
            }
            recv_waker.map(Waker::wake);
        } else {
            // receiver locked the waker. it will see STATE_LOADED when it unlocks
            // and take the value immediately. call our waker to proceed to flush()
            if let Some(waker) = waker {
                waker.wake_by_ref();
            }
            // note: receiver will also reset the WAKE flag appropriately
        }
        Ok(())
    }

    pub fn flush(&self, waker: &Waker, finalize: bool) -> Poll<(Option<T>, bool)> {
        let mut prev = self.state.load(Ordering::Relaxed);
        let mut locked = false;
        loop {
            if prev & STATE_LOADED == 0 {
                if locked {
                    // reset state
                    let mut remove = STATE_WLOCK | STATE_WAKE;
                    if finalize {
                        remove |= STATE_ACTIVE;
                    }
                    self.state.fetch_and(!remove, Ordering::Relaxed);
                } else {
                    // using acquire to sync with the read
                    prev = if finalize {
                        self.state.fetch_and(!STATE_ACTIVE, Ordering::Acquire)
                    } else {
                        self.state.load(Ordering::Acquire)
                    };
                }
                return Poll::Ready((None, prev & STATE_ACTIVE == 0));
            }
            if prev & STATE_ACTIVE == 0 {
                // receiver dropped, leaving value. there must not be a waker
                if !finalize {
                    // clear LOADED flag for when the writer drops
                    self.state.store(STATE_DONE, Ordering::Relaxed);
                }
                return Poll::Ready((Some(unsafe { self.value.load() }), true));
            }
            if prev & STATE_RLOCK != 0 {
                // read in progress, just call waker
                // reader would have also set WLOCK and WAKE, so leave them alone
                waker.wake_by_ref();
                return Poll::Pending;
            }
            if locked {
                if prev & STATE_WAKE != 0 {
                    // take previous send waker
                    unsafe { self.waker.clear() };
                }
                unsafe { self.waker.store(waker.clone()) };
                return Poll::Pending;
            } else {
                prev = match self.state.compare_exchange(
                    prev,
                    prev | STATE_WLOCK | STATE_WAKE,
                    Ordering::Acquire,
                    Ordering::Acquire,
                ) {
                    Ok(s) => {
                        locked = true;
                        s
                    }
                    Err(s) if s & STATE_WLOCK != 0 => {
                        // read in progress, just call waker
                        waker.wake_by_ref();
                        return Poll::Pending;
                    }
                    Err(s) => {
                        // read completed or reader dropped, retry
                        s
                    }
                };
            }
        }
    }

    pub fn read(&self, mut waker: Option<&Waker>, or_cancel: bool) -> Poll<(Option<T>, bool)> {
        let mut prev = self
            .state
            .fetch_or(STATE_RLOCK | STATE_WLOCK | STATE_WAKE, Ordering::Acquire);
        let (locked, prev_wake) = (prev & STATE_WLOCK == 0, prev & STATE_WAKE != 0);
        if prev & STATE_LOADED != 0 {
            let value = unsafe { self.value.load() };
            let send_waker = if locked && prev_wake {
                Some(unsafe { self.waker.load() })
            } else {
                None
            };
            if locked {
                prev = self.state.fetch_and(
                    // if finalize { STATE_DONE } else { STATE_ACTIVE },
                    !(STATE_LOADED | STATE_WLOCK | STATE_WAKE | STATE_RLOCK),
                    Ordering::Release,
                );
            } else {
                // leave WLOCK and WAKE for the sender to control
                prev = self
                    .state
                    .fetch_and(!(STATE_RLOCK | STATE_LOADED), Ordering::Release);
            }
            send_waker.map(Waker::wake);
            Poll::Ready((Some(value), prev & STATE_ACTIVE == 0))
        } else {
            // a value was loaded

            // WLOCK without LOADED should only be set by read()
            assert_eq!(locked, true, "Should not be locked");

            if prev_wake {
                // drop previous receive waker
                unsafe { self.waker.clear() };
            }
            if prev & STATE_ACTIVE != 0 {
                if let Some(waker) = waker.take() {
                    unsafe { self.waker.store(waker.clone()) };
                    prev = self
                        .state
                        .fetch_and(!(STATE_RLOCK | STATE_WLOCK), Ordering::Release);
                    if prev & STATE_ACTIVE == 0 {
                        // drop waker we just stored
                        unsafe { self.waker.clear() };
                    }
                } else {
                    let mut remove = STATE_RLOCK | STATE_WLOCK | STATE_WAKE;
                    if or_cancel {
                        remove |= STATE_ACTIVE;
                    }
                    prev = self.state.fetch_and(!remove, Ordering::Release);
                }
            }
            if prev & STATE_LOADED != 0 {
                // sender loaded a value while we were locked - retry
                self.read(None, or_cancel)
            } else if prev & STATE_ACTIVE == 0 {
                // sender dropped
                Poll::Ready((None, true))
            } else {
                Poll::Pending
            }
        }
    }

    pub fn drop_one_side(&self, is_writer: bool) -> bool {
        let mut prev = self
            .state
            .fetch_or(STATE_WLOCK | STATE_WAKE, Ordering::Acquire);
        if prev & STATE_ACTIVE == 0 {
            // reader already dropped
            if prev & STATE_LOADED != 0 {
                unsafe { self.value.clear() };
            }
            if prev & STATE_WAKE != 0 {
                unsafe { self.waker.clear() };
            }
            true
        } else {
            if prev & STATE_WLOCK == 0 {
                let waker = if prev & STATE_WAKE != 0 {
                    if (prev & STATE_LOADED != 0) ^ is_writer {
                        // call waker, it's not ours
                        Some(unsafe { self.waker.load() })
                    } else {
                        // clear waker, it's ours
                        unsafe { self.waker.clear() };
                        None
                    }
                } else {
                    None
                };
                prev = self.state.fetch_and(
                    !(STATE_WLOCK | STATE_WAKE | STATE_ACTIVE),
                    Ordering::Release,
                );
                waker.map(Waker::wake);
            } else {
                prev = self.state.fetch_and(!STATE_ACTIVE, Ordering::Release);
            }
            if prev & STATE_ACTIVE == 0 {
                // other side beat us to the drop
                unsafe { self.value.clear() };
                true
            } else {
                false
            }
        }
    }

    pub fn wait_read(&self) -> (Option<T>, bool) {
        // poll once without creating a listener in case the value is ready
        if let Poll::Ready(result) = self.read(None, false) {
            result
        }
        // poll with a listener
        else if let Poll::Ready(result) =
            block_on_poll(|cx| self.read(Some(cx.waker()), false), None)
        {
            result
        } else {
            unreachable!()
        }
    }

    pub fn wait_read_timeout(&self, timeout: Option<Instant>) -> (Option<T>, bool) {
        // poll once without creating a listener in case the value is ready
        if let Poll::Ready(result) = self.read(None, false) {
            return result;
        }
        // poll with a listener
        match block_on_poll(|cx| self.read(Some(cx.waker()), false), timeout) {
            Poll::Ready(result) => result,
            Poll::Pending => {
                // cancel read - return value if any was stored
                if let Poll::Ready(r) = self.read(None, true) {
                    r
                } else {
                    (None, false)
                }
            }
        }
    }
}

impl<T> Debug for Channel<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("done", &self.is_done())
            .finish()
    }
}

impl<T> panic::RefUnwindSafe for Channel<T> {}

/// A [`Future`] which resolves once a write has completed.
#[must_use = "Flush does nothing unless you `.await` or poll it"]
#[derive(Debug)]
pub struct Flush<'a, T> {
    channel: Option<BoxPtr<Channel<T>>>,
    value: Option<T>,
    finalize: bool,
    _marker: PhantomData<&'a mut T>,
}

impl<T> Drop for Flush<'_, T> {
    fn drop(&mut self) {
        if let Some(channel) = self.channel.take() {
            if self.finalize {
                if unsafe { channel.to_ref() }.drop_one_side(true) {
                    drop(unsafe { channel.into_box() });
                }
            }
            // else: try to take the value out of the channel? otherwise
            // it can panic on the next send.
        }
    }
}

impl<T> Future for Flush<'_, T> {
    type Output = Result<(), T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(value) = self.value.take() {
            let channel = self.channel.take().unwrap();
            if let Err(value) =
                unsafe { channel.to_ref() }.write(value, Some(cx.waker()), self.finalize)
            {
                if self.finalize {
                    drop(unsafe { channel.into_box() });
                }
                Poll::Ready(Err(value))
            } else {
                self.channel.replace(channel);
                Poll::Pending
            }
        } else if let Some(channel) = self.channel.take() {
            if let Poll::Ready((result, dropped)) =
                unsafe { channel.to_ref() }.flush(cx.waker(), self.finalize)
            {
                if dropped && self.finalize {
                    drop(unsafe { channel.into_box() });
                }
                Poll::Ready(result.map(Err).unwrap_or(Ok(())))
            } else {
                self.channel.replace(channel);
                Poll::Pending
            }
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

impl<T> FusedFuture for Flush<'_, T> {
    #[inline]
    fn is_terminated(&self) -> bool {
        self.channel.is_none()
    }
}

impl<T> Unpin for Flush<'_, T> {}

/// Created by [`send_once()`] and used to dispatch a single value
/// to an associated [`ReceiveOnce`] instance.
#[derive(Debug)]
pub struct SendOnce<T> {
    channel: BoxPtr<Channel<T>>,
}

impl<T> SendOnce<T> {
    /// Check if the receiver has already been dropped.
    #[inline]
    pub fn is_canceled(&self) -> bool {
        unsafe { self.channel.to_ref() }.is_done()
    }

    /// Dispatch the result and consume the [`SendOnce`].
    pub fn send_nowait(self, value: T) -> Result<(), T> {
        let channel = ManuallyDrop::new(self).channel;
        unsafe { channel.to_ref() }
            .write(value, None, true)
            .map_err(|value| {
                drop(unsafe { channel.into_box() });
                value
            })
    }

    /// Load a value to be sent, returning a [`Future`] which resolves when
    /// the value is received or the [`ReceiveOnce`] is dropped.
    #[inline]
    pub fn send(self, value: T) -> Flush<'static, T> {
        Flush {
            channel: Some(ManuallyDrop::new(self).channel),
            value: Some(value),
            finalize: true,
            _marker: PhantomData,
        }
    }
}

impl<T> Drop for SendOnce<T> {
    fn drop(&mut self) {
        unsafe {
            if self.channel.to_ref().drop_one_side(true) {
                drop(self.channel.into_box());
            }
        }
    }
}

impl<T> Unpin for SendOnce<T> {}

/// Created by [`send_once()`] and used to receive a single value from
/// an associated [`SendOnce`] instance.
#[derive(Debug)]
pub struct ReceiveOnce<T> {
    channel: Option<BoxPtr<Channel<T>>>,
}

impl<T> ReceiveOnce<T> {
    /// Safely cancel the receive operation, consuming the [`ReceiveOnce`].
    pub fn cancel(self) -> Option<T> {
        ManuallyDrop::new(self).channel.take().and_then(|channel| {
            if let Poll::Ready((result, dropped)) = unsafe { channel.to_ref() }.read(None, true) {
                if dropped {
                    drop(unsafe { channel.into_box() });
                }
                result
            } else {
                // sender has not dropped or produced a value
                None
            }
        })
    }

    /// Try to receive the value, consuming the [`ReceiveOnce`] if the value
    /// has been loaded or the [`SendOnce`] has been dropped.
    #[inline]
    pub fn try_recv(&mut self) -> Option<Result<T, RecvError>> {
        match self.poll_result(None) {
            Poll::Ready(r) => Some(r),
            Poll::Pending => None,
        }
    }

    /// Block the current thread until a value is received or the [`SendOnce`] is dropped.
    pub fn recv(self) -> Result<T, RecvError> {
        if let Some(channel) = ManuallyDrop::new(self).channel.take() {
            let (result, dropped) = unsafe { channel.to_ref() }.wait_read();
            if dropped {
                drop(unsafe { channel.into_box() });
            }
            result.ok_or(RecvError::Incomplete)
        } else {
            Err(RecvError::Terminated)
        }
    }

    /// Block the current thread until a value is received or the [`SendOnce`] is dropped,
    /// returning `Err(Self)` if a timeout is reached.
    pub fn recv_timeout(&mut self, timeout: impl Expiry) -> Result<T, RecvError> {
        if let Some(channel) = self.channel.take() {
            let (result, dropped) =
                unsafe { channel.to_ref() }.wait_read_timeout(timeout.into_opt_instant());
            if dropped {
                drop(unsafe { channel.into_box() });
                result.ok_or(RecvError::Incomplete)
            } else if let Some(result) = result {
                Ok(result)
            } else {
                self.channel.replace(channel);
                Err(RecvError::TimedOut)
            }
        } else {
            Err(RecvError::Terminated)
        }
    }

    // poll for the result (internal)
    fn poll_result(&mut self, waker: Option<&Waker>) -> Poll<Result<T, RecvError>> {
        if let Some(channel) = self.channel {
            if let Poll::Ready((result, dropped)) = unsafe { channel.to_ref() }.read(waker, false) {
                if dropped {
                    self.channel.take();
                    drop(unsafe { channel.into_box() });
                }
                Poll::Ready(result.ok_or(RecvError::Incomplete))
            } else {
                // sender has not dropped or produced a value
                Poll::Pending
            }
        } else {
            Poll::Ready(Err(RecvError::Terminated))
        }
    }
}

impl<T> Drop for ReceiveOnce<T> {
    fn drop(&mut self) {
        if let Some(channel) = self.channel.take() {
            if unsafe { channel.to_ref() }.drop_one_side(false) {
                drop(unsafe { channel.into_box() });
            }
        }
    }
}

impl<T> Future for ReceiveOnce<T> {
    type Output = Result<T, RecvError>;

    #[inline]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.poll_result(Some(cx.waker()))
    }
}

impl<T> FusedFuture for ReceiveOnce<T> {
    #[inline]
    fn is_terminated(&self) -> bool {
        self.channel
            .as_ref()
            .map(|c| unsafe { c.to_ref() }.is_done())
            .unwrap_or(true)
    }
}

impl<T> Stream for ReceiveOnce<T> {
    type Item = T;

    #[inline]
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.poll_result(Some(cx.waker())).map(Result::ok)
    }
}

impl<T> FusedStream for ReceiveOnce<T> {
    #[inline]
    fn is_terminated(&self) -> bool {
        self.channel
            .as_ref()
            .map(|c| unsafe { c.to_ref() }.is_done())
            .unwrap_or(true)
    }
}

impl<T> Unpin for ReceiveOnce<T> {}

/// Created by [`channel()`] and used to dispatch a stream of values to a
/// an associated [`Receiver`].
#[derive(Debug)]
pub struct Sender<T> {
    channel: Option<BoxPtr<Channel<T>>>,
}

impl<T> Sender<T> {
    /// Check if the receiver has already been dropped.
    #[inline]
    pub fn is_canceled(&self) -> bool {
        self.channel
            .map(|c| unsafe { c.to_ref() }.is_done())
            .unwrap_or(true)
    }

    /// Send a single result and consume the [`Sender`].
    pub fn send_nowait(self, value: T) -> Result<(), T> {
        if let Some(channel) = ManuallyDrop::new(self).channel {
            unsafe { channel.to_ref() }
                .write(value, None, true)
                .map_err(|value| {
                    drop(unsafe { channel.into_box() });
                    value
                })
        } else {
            Err(value)
        }
    }

    /// Send the next result, returning a `Future` which can be used to await
    /// its delivery.
    #[inline]
    pub fn send(&mut self, value: T) -> Flush<'_, T> {
        Flush {
            channel: self.channel,
            value: Some(value),
            finalize: false,
            _marker: PhantomData,
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if let Some(channel) = self.channel.take() {
            if unsafe { channel.to_ref() }.drop_one_side(true) {
                drop(unsafe { channel.into_box() });
            }
        }
    }
}

impl<T> Unpin for Sender<T> {}

/// Created by [`channel()`] and used to receive a stream of values from
/// an associated [`Sender`].
#[derive(Debug)]
pub struct Receiver<T> {
    channel: Option<BoxPtr<Channel<T>>>,
}

impl<T> Receiver<T> {
    /// Safely cancel the receive operation, consuming the [`Receiver`].
    pub fn cancel(self) -> Option<T> {
        if let Some(channel) = ManuallyDrop::new(self).channel.take() {
            if let Poll::Ready((result, dropped)) = unsafe { channel.to_ref() }.read(None, true) {
                if dropped {
                    drop(unsafe { channel.into_box() });
                }
                result
            } else {
                // sender has not dropped or produced a value
                None
            }
        } else {
            None
        }
    }

    /// Try to receive the next value from the stream.
    pub fn try_recv(&mut self) -> Poll<Option<T>> {
        if let Some(channel) = self.channel.take() {
            if let Poll::Ready((result, dropped)) = unsafe { channel.to_ref() }.read(None, false) {
                if dropped {
                    drop(unsafe { channel.into_box() });
                } else {
                    self.channel.replace(channel);
                }
                Poll::Ready(result)
            } else {
                self.channel.replace(channel);
                Poll::Pending
            }
        } else {
            Poll::Ready(None)
        }
    }

    /// Block the current thread on the next result from the [`Sender`], returning
    /// [`None`] if the sender has been dropped.
    pub fn wait_next(&mut self) -> Option<T> {
        if let Some(channel) = self.channel.take() {
            let (result, dropped) = unsafe { channel.to_ref() }.wait_read();
            if dropped {
                drop(unsafe { channel.into_box() });
            } else {
                self.channel.replace(channel);
            }
            result
        } else {
            None
        }
    }

    /// Block the current thread on the next result from the [`Sender`], returning
    /// `Ok(None)`] if the sender has been dropped and `Err(Incomplete)` if the
    /// provided timeout is reached.
    pub fn wait_next_timeout(&mut self, timeout: impl Expiry) -> Result<T, RecvError> {
        if let Some(channel) = self.channel.take() {
            let (result, dropped) =
                unsafe { channel.to_ref() }.wait_read_timeout(timeout.into_opt_instant());
            if dropped {
                drop(unsafe { channel.into_box() });
                result.ok_or(RecvError::Incomplete)
            } else if let Some(result) = result {
                Ok(result)
            } else {
                self.channel.replace(channel);
                Err(RecvError::TimedOut)
            }
        } else {
            Err(RecvError::Terminated)
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if let Some(channel) = self.channel.take() {
            if unsafe { channel.to_ref() }.drop_one_side(false) {
                drop(unsafe { channel.into_box() });
            }
        }
    }
}

impl<T> Stream for Receiver<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        if let Some(channel) = self.channel.clone() {
            unsafe { channel.to_ref() }
                .read(Some(cx.waker()), false)
                .map(|(result, dropped)| {
                    if dropped {
                        self.channel.take();
                        drop(unsafe { channel.into_box() });
                    }
                    result
                })
        } else {
            Poll::Ready(None)
        }
    }
}

impl<T> FusedStream for Receiver<T> {
    #[inline]
    fn is_terminated(&self) -> bool {
        self.channel
            .as_ref()
            .map(|c| unsafe { c.to_ref() }.is_done())
            .unwrap_or(true)
    }
}

impl<T> Unpin for Receiver<T> {}
