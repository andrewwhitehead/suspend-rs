extern crate alloc;

use alloc::sync::Arc;
use core::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};
#[cfg(feature = "std")]
use std::thread;

use futures_core::{FusedStream, Stream};
use futures_task::{waker_ref, ArcWake};

use suspend_channel::{channel, TrySendError};

#[cfg(feature = "std")]
use suspend_channel::StreamIterExt;

#[cfg(feature = "std")]
use suspend_core::thread::block_on;

mod utils;
use utils::TestDrop;

struct TestWaker {
    calls: AtomicUsize,
}

impl TestWaker {
    pub fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    pub fn count(&self) -> usize {
        return self.calls.load(Ordering::Acquire);
    }
}

impl ArcWake for TestWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn channel_send_receive_poll() {
    let (mut sender, mut receiver) = channel();
    let (message, drops) = TestDrop::new_pair();
    let waker = Arc::new(TestWaker::new());
    let wr = waker_ref(&waker);
    let mut cx = Context::from_waker(&wr);
    assert_eq!(Pin::new(&mut receiver).poll_next(&mut cx), Poll::Pending);
    assert_eq!(waker.count(), 0);
    assert_eq!(sender.try_send(message), Ok(()));
    assert_eq!(waker.count(), 1);
    assert_eq!(receiver.is_terminated(), false);
    assert_eq!(drops.count(), 0);
    if let Poll::Ready(Some(result)) = Pin::new(&mut receiver).poll_next(&mut cx) {
        assert_eq!(drops.count(), 0);
        drop(result);
        assert_eq!(drops.count(), 1);
        assert_eq!(waker.count(), 1);
        assert_eq!(receiver.is_terminated(), false);
        assert_eq!(Pin::new(&mut receiver).poll_next(&mut cx), Poll::Pending);
        assert_eq!(waker.count(), 1);
        assert_eq!(drops.count(), 1);
        drop(sender);
        assert_eq!(receiver.is_terminated(), true);
        assert_eq!(
            Pin::new(&mut receiver).poll_next(&mut cx),
            Poll::Ready(None)
        );
        drop(receiver);
        assert_eq!(drops.count(), 1);
    } else {
        panic!("Error receiving payload")
    }
}

#[cfg(feature = "std")]
#[test]
fn channel_send_receive_block_on() {
    let (mut sender, mut receiver) = channel();
    assert_eq!(sender.try_send(1u32), Ok(()));
    assert_eq!(block_on(receiver.stream_next()), Some(1u32));
}

#[cfg(feature = "std")]
#[test]
fn channel_send_receive_wait_next() {
    let (mut sender, mut receiver) = channel();
    assert_eq!(sender.try_send(1u32), Ok(()));
    assert_eq!(receiver.wait_next(), Some(1u32));
}

#[cfg(feature = "std")]
#[test]
fn channel_send_receive_thread() {
    let (mut sender, mut receiver) = channel();
    let ta = thread::spawn(move || sender.try_send(1u32).unwrap());
    assert_eq!(block_on(receiver.stream_next()), Some(1u32));
    ta.join().unwrap();
}

#[cfg(feature = "std")]
#[test]
fn channel_send_receive_forward_thread() {
    let (mut sender0, mut receiver0) = channel();
    let (mut sender1, mut receiver1) = channel();
    let ta = thread::spawn(move || {
        sender1
            .try_send(block_on(receiver0.stream_next()).unwrap())
            .unwrap()
    });
    let tb = thread::spawn(move || sender0.try_send(1u32).unwrap());
    assert_eq!(block_on(receiver1.stream_next()), Some(1u32));
    ta.join().unwrap();
    tb.join().unwrap();
}

#[cfg(feature = "std")]
#[test]
fn channel_send_receive_thread_multiple() {
    let (mut sender, receiver) = channel::<i32>();
    let ops = thread::spawn(move || {
        for idx in 0..100 {
            assert_eq!(block_on(sender.send(idx)), Ok(()));
        }
    });
    let mut next = 0;
    for found in receiver.stream_into_iter() {
        assert_eq!(found, next);
        next += 1;
    }
    ops.join().unwrap();
}

#[test]
fn channel_sender_dropped() {
    let (sender, mut receiver) = channel::<u32>();
    let waker = Arc::new(TestWaker::new());
    let wr = waker_ref(&waker);
    let mut cx = Context::from_waker(&wr);
    assert_eq!(Pin::new(&mut receiver).poll_next(&mut cx), Poll::Pending);
    drop(sender);
    assert_eq!(waker.count(), 1);
    assert_eq!(
        Pin::new(&mut receiver).poll_next(&mut cx),
        Poll::Ready(None)
    );
    assert_eq!(waker.count(), 1);
}

#[test]
fn channel_receiver_dropped_early() {
    let (mut sender, receiver) = channel();
    drop(receiver);
    assert_eq!(sender.try_send(1u32), Err(TrySendError::Disconnected(1u32)));
}

#[test]
fn channel_receiver_dropped_incomplete() {
    let (mut sender, receiver) = channel();
    let (message, drops) = TestDrop::new_pair();
    sender.try_send(message).unwrap();
    assert_eq!(drops.count(), 0);
    //assert!(receiver.wait().is_ok());
    drop(receiver);
    assert_eq!(drops.count(), 0);
    drop(sender);
    assert_eq!(drops.count(), 1);
}

#[test]
fn channel_receiver_dropped_complete() {
    let (mut sender, mut receiver) = channel();
    let (message, drops) = TestDrop::new_pair();
    sender.try_send(message).unwrap();
    let result = receiver.try_recv();
    assert_eq!(drops.count(), 0);
    drop(result);
    assert_eq!(drops.count(), 1);
    drop(receiver);
    assert_eq!(drops.count(), 1);
}

#[cfg(feature = "std")]
#[test]
fn channel_receiver_block_on() {
    let (mut sender, mut receiver) = channel::<u32>();
    sender.try_send(5).unwrap();
    assert_eq!(block_on(receiver.stream_next()), Some(5));
}

#[cfg(feature = "std")]
#[test]
fn channel_receiver_wait_next() {
    let (mut sender, mut receiver) = channel::<u32>();
    sender.try_send(5).unwrap();
    assert_eq!(receiver.wait_next(), Some(5));
}

#[test]
fn channel_receiver_stream_one() {
    let (mut sender, mut receiver) = channel::<u32>();
    sender.try_send(5).unwrap();
    assert_eq!(receiver.is_terminated(), false);
    assert_eq!(receiver.try_recv(), Poll::Ready(Some(5)));
    assert_eq!(receiver.is_terminated(), false);
    drop(sender);
    assert_eq!(receiver.is_terminated(), true);
    assert_eq!(receiver.try_recv(), Poll::Ready(None));
    assert_eq!(receiver.is_terminated(), true);
}

#[test]
fn channel_receiver_stream_empty() {
    let (sender, mut receiver) = channel::<u32>();
    drop(sender);
    assert_eq!(receiver.is_terminated(), true);
    assert_eq!(receiver.try_recv(), Poll::Ready(None));
    assert_eq!(receiver.is_terminated(), true);
}

#[test]
fn channel_receiver_cancel_early() {
    let (mut sender, receiver) = channel::<u32>();
    assert_eq!(receiver.cancel(), None);
    assert!(sender.try_send(5).is_err());
}

#[test]
fn channel_receiver_cancel_late() {
    let (mut sender, receiver) = channel::<u32>();
    sender.try_send(5).unwrap();
    assert_eq!(receiver.cancel(), Some(5));
}
