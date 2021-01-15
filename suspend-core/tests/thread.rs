#![cfg(feature = "std")]

use std::task::Poll;
use std::thread;
use std::time::Duration;

use futures_lite::future;

use suspend_core::thread::{block_on, block_on_poll, park_thread};

#[test]
fn park_thread_basic() {
    assert_eq!(
        park_thread(
            |notifier| {
                thread::spawn(move || notifier.notify());
            },
            Duration::from_millis(100)
        )
        .timed_out(),
        true
    );
}

#[test]
fn park_thread_timeout() {
    assert_eq!(
        park_thread(|_notifier| {}, Duration::from_millis(100)).timed_out(),
        true
    );
}

#[test]
fn block_on_ready() {
    assert_eq!(block_on(async { true }), true);
}

#[test]
fn block_on_timeout() {
    assert_eq!(
        block_on_poll(|_cx| Poll::<bool>::Pending, Duration::from_millis(100)),
        Poll::Pending
    );
}

#[test]
fn block_on_repoll() {
    let mut ready = false;
    assert_eq!(
        block_on(future::poll_fn(|cx| {
            if ready {
                Poll::Ready(true)
            } else {
                ready = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })),
        true
    );
}

#[test]
fn block_on_repoll_delay() {
    let mut ready = false;
    assert_eq!(
        block_on_poll(
            |cx| {
                if ready {
                    Poll::Ready(true)
                } else {
                    ready = true;
                    let waker = cx.waker().clone();
                    thread::spawn(|| {
                        thread::sleep(Duration::from_millis(50));
                        waker.wake()
                    });
                    Poll::Pending
                }
            },
            Duration::from_millis(200)
        ),
        Poll::Ready(true)
    );
}
