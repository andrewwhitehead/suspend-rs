use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::task::{Context, Poll};
use std::thread;

use futures_core::{FusedStream, Stream};
use futures_task::{waker_ref, ArcWake};

use suspend_channel::{channel, StreamIterExt};
use suspend_core::listen::block_on;

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
    let (sender, mut receiver) = channel();
    let (message, drops) = TestDrop::new_pair();
    let waker = Arc::new(TestWaker::new());
    let wr = waker_ref(&waker);
    let mut cx = Context::from_waker(&wr);
    assert_eq!(Pin::new(&mut receiver).poll_next(&mut cx), Poll::Pending);
    assert_eq!(waker.count(), 0);
    assert_eq!(sender.send_nowait(message), Ok(()));
    assert_eq!(waker.count(), 1);
    assert_eq!(receiver.is_terminated(), false);
    assert_eq!(drops.count(), 0);
    if let Poll::Ready(Some(result)) = Pin::new(&mut receiver).poll_next(&mut cx) {
        assert_eq!(drops.count(), 0);
        drop(result);
        assert_eq!(drops.count(), 1);
        assert_eq!(waker.count(), 1);
        assert_eq!(receiver.is_terminated(), true);
        assert_eq!(
            Pin::new(&mut receiver).poll_next(&mut cx),
            Poll::Ready(None)
        );
        assert_eq!(waker.count(), 1);
        assert_eq!(drops.count(), 1);
        drop(receiver);
        assert_eq!(drops.count(), 1);
    } else {
        panic!("Error receiving payload")
    }
}

#[test]
fn channel_send_receive_block_on() {
    let (sender, mut receiver) = channel();
    assert_eq!(sender.send_nowait(1u32), Ok(()));
    assert_eq!(block_on(receiver.stream_next()), Some(1u32));
}

#[test]
fn channel_send_receive_wait_next() {
    let (sender, mut receiver) = channel();
    assert_eq!(sender.send_nowait(1u32), Ok(()));
    assert_eq!(receiver.wait_next(), Some(1u32));
}

#[test]
fn channel_send_receive_thread() {
    let (sender, mut receiver) = channel();
    let ta = thread::spawn(move || sender.send_nowait(1u32).unwrap());
    assert_eq!(block_on(receiver.stream_next()), Some(1u32));
    ta.join().unwrap();
}

#[test]
fn channel_send_receive_forward_thread() {
    let (sender0, mut receiver0) = channel();
    let (sender1, mut receiver1) = channel();
    let ta = thread::spawn(move || {
        sender1
            .send_nowait(block_on(receiver0.stream_next()).unwrap())
            .unwrap()
    });
    let tb = thread::spawn(move || sender0.send_nowait(1u32).unwrap());
    assert_eq!(block_on(receiver1.stream_next()), Some(1u32));
    ta.join().unwrap();
    tb.join().unwrap();
}

#[test]
fn channel_send_receive_thread_multiple() {
    let (mut sender, receiver) = channel::<i32>();
    let ops = thread::spawn(move || {
        for idx in 0..1000 {
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
    let (sender, receiver) = channel();
    drop(receiver);
    assert_eq!(sender.send_nowait(1u32), Err(1u32));
}

#[test]
fn channel_receiver_dropped_incomplete() {
    let (sender, receiver) = channel();
    let (message, drops) = TestDrop::new_pair();
    sender.send_nowait(message).unwrap();
    assert_eq!(drops.count(), 0);
    //assert!(receiver.wait().is_ok());
    drop(receiver);
    assert_eq!(drops.count(), 1);
}

#[test]
fn channel_receiver_dropped_complete() {
    let (sender, mut receiver) = channel();
    let (message, drops) = TestDrop::new_pair();
    sender.send_nowait(message).unwrap();
    let result = block_on(receiver.stream_next()).unwrap();
    assert_eq!(drops.count(), 0);
    drop(result);
    assert_eq!(drops.count(), 1);
    drop(receiver);
    assert_eq!(drops.count(), 1);
}

#[test]
fn channel_receiver_block_on() {
    let (sender, mut receiver) = channel::<u32>();
    sender.send_nowait(5).unwrap();
    assert_eq!(block_on(receiver.stream_next()), Some(5));
}

#[test]
fn channel_receiver_wait_next() {
    let (sender, mut receiver) = channel::<u32>();
    sender.send_nowait(5).unwrap();
    assert_eq!(receiver.wait_next(), Some(5));
}

#[test]
fn channel_receiver_stream_one() {
    let (sender, mut receiver) = channel::<u32>();
    sender.send_nowait(5).unwrap();
    assert_eq!(receiver.is_terminated(), false);
    assert_eq!(block_on(receiver.stream_next()), Some(5));
    assert_eq!(receiver.is_terminated(), true);
    assert_eq!(block_on(receiver.stream_next()), None);
    assert_eq!(receiver.is_terminated(), true);
}

#[test]
fn channel_receiver_stream_empty() {
    let (sender, mut receiver) = channel::<u32>();
    drop(sender);
    assert_eq!(receiver.is_terminated(), true);
    assert_eq!(block_on(receiver.stream_next()), None);
    assert_eq!(receiver.is_terminated(), true);
}

#[test]
fn channel_receiver_cancel_early() {
    let (sender, receiver) = channel::<u32>();
    assert_eq!(receiver.cancel(), None);
    assert!(sender.send_nowait(5).is_err());
}

#[test]
fn channel_receiver_cancel_late() {
    let (sender, receiver) = channel::<u32>();
    sender.send_nowait(5).unwrap();
    assert_eq!(receiver.cancel(), Some(5));
}
