//! Thread parking operations

use core::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use std::thread;

use crate::{
    listen::{Listener, ListenerGuard, Notifier},
    types::{Expiry, ParkResult},
};

thread_local! {
    pub(crate) static THREAD_LISTEN: RefCell<(Listener, Waker)> = {
        let listener = Listener::new();
        let waker = listener.waker();
        RefCell::new((listener, waker))
    };
}

/// Block the current thread on the result of a [`Future`].
pub fn block_on<'s, T>(fut: impl Future<Output = T>) -> T {
    pin!(fut);
    with_listener(|guard, waker| {
        let mut cx = Context::from_waker(waker);
        loop {
            if let Poll::Ready(result) = fut.as_mut().poll(&mut cx) {
                break result;
            }
            guard.park(None).unwrap();
        }
    })
}

/// Block the current thread on the result of a poll function, with an optional timeout.
/// A [`None`] value is returned if the timeout is reached.
pub fn block_on_poll<'s, T>(
    mut poll: impl FnMut(&mut Context<'_>) -> Poll<T>,
    timeout: impl Into<Expiry>,
) -> Poll<T> {
    let timeout = timeout.into();
    with_listener(|guard, waker| {
        let mut cx = Context::from_waker(waker);
        let mut repeat = 0usize;
        loop {
            let result = poll(&mut cx);
            if result.is_ready() {
                break result;
            }
            match guard.park(timeout).unwrap() {
                ParkResult::Skipped => {
                    repeat += 1;
                    if repeat >= 5 {
                        // seem to be effectively in a spin loop, back off polling
                        thread::yield_now();
                    }
                }
                ParkResult::Unparked => {
                    repeat = 0;
                }
                ParkResult::TimedOut => break result,
            }
        }
    })
}

/// Block the current thread on the result of a [`Future`] + [`Unpin`], with an
/// optional timeout. A [`None`] value is returned if the timeout is reached.
#[inline]
pub fn block_on_unpin<'s, F, T>(mut fut: F, timeout: impl Into<Expiry>) -> Result<T, F>
where
    F: Future<Output = T> + Unpin,
{
    let timeout = timeout.into();
    match block_on_poll(|cx| Pin::new(&mut fut).poll(cx), timeout) {
        Poll::Ready(r) => Ok(r),
        Poll::Pending => Err(fut),
    }
}

/// Park the current thread until notified, with an optional timeout.
/// If a timeout is specified and reached before there is a notification, then
/// a `false` value is returned. Note that this function is susceptible to
/// spurious notifications. Use a dedicated [`Listener`] instance if this is
/// undesirable.
pub fn park_thread<'s>(f: impl FnOnce(Notifier), timeout: impl Into<Expiry>) -> ParkResult {
    let timeout = timeout.into();
    with_listener(|guard, _waker| {
        f(guard.notifier());
        guard.park(timeout).unwrap()
    })
}

pub(crate) fn with_listener<T>(f: impl FnOnce(&mut ListenerGuard<'_>, &Waker) -> T) -> T {
    THREAD_LISTEN.with(|cached| {
        if let Ok(mut borrowed) = cached.try_borrow_mut() {
            let (listen, waker) = &mut *borrowed;
            let mut guard = listen.get_mut();
            f(&mut guard, waker)
        } else {
            // thread listener in use, create a new one
            let mut listen = Listener::new();
            let mut guard = listen.get_mut();
            let waker = guard.waker();
            f(&mut guard, &waker)
        }
    })
}
