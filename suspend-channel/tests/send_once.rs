use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::task::{Context, Poll};
use std::thread;

use futures_core::FusedFuture;
use futures_task::{waker_ref, ArcWake};

use suspend_channel::{send_once, RecvError, StreamNext};
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
fn send_once_receive_poll() {
    let (sender, mut receiver) = send_once();
    let (message, drops) = TestDrop::new_pair();
    let waker = Arc::new(TestWaker::new());
    let wr = waker_ref(&waker);
    let mut cx = Context::from_waker(&wr);
    assert_eq!(Pin::new(&mut receiver).poll(&mut cx), Poll::Pending);
    assert_eq!(waker.count(), 0);
    assert_eq!(sender.send_nowait(message), Ok(()));
    assert_eq!(waker.count(), 1);
    assert_eq!(receiver.is_terminated(), false);
    assert_eq!(drops.count(), 0);
    if let Poll::Ready(Ok(result)) = Pin::new(&mut receiver).poll(&mut cx) {
        assert_eq!(drops.count(), 0);
        drop(result);
        assert_eq!(drops.count(), 1);
        assert_eq!(waker.count(), 1);
        assert_eq!(receiver.is_terminated(), true);
        assert_eq!(
            Pin::new(&mut receiver).poll(&mut cx),
            Poll::Ready(Err(RecvError::Terminated))
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
fn send_once_receive_block_on() {
    let (sender, receiver) = send_once();
    assert_eq!(sender.send_nowait(1u32), Ok(()));
    assert_eq!(block_on(receiver), Ok(1u32));
}

#[test]
fn send_once_receive_wait() {
    let (sender, receiver) = send_once();
    assert_eq!(sender.send_nowait(1u32), Ok(()));
    assert_eq!(receiver.recv(), Ok(1u32));
}

#[test]
fn send_once_threaded() {
    let (sender0, receiver0) = send_once();
    let (sender1, receiver1) = send_once();
    thread::spawn(move || sender1.send_nowait(block_on(receiver0).unwrap()).unwrap());
    thread::spawn(move || sender0.send_nowait(1u32).unwrap());
    assert_eq!(block_on(receiver1), Ok(1u32));
}

#[test]
fn send_once_sender_dropped() {
    let (sender, mut receiver) = send_once::<u32>();
    let waker = Arc::new(TestWaker::new());
    let wr = waker_ref(&waker);
    let mut cx = Context::from_waker(&wr);
    assert_eq!(Pin::new(&mut receiver).poll(&mut cx), Poll::Pending);
    drop(sender);
    assert_eq!(waker.count(), 1, "Receiver waker not called");
    assert_eq!(
        Pin::new(&mut receiver).poll(&mut cx),
        Poll::Ready(Err(RecvError::Incomplete))
    );
    assert_eq!(waker.count(), 1);
}

#[test]
fn send_once_receiver_dropped_early() {
    let (sender, receiver) = send_once();
    drop(receiver);
    assert_eq!(sender.send_nowait(1u32), Err(1u32));
}

#[test]
fn send_once_receiver_dropped_incomplete() {
    let (sender, receiver) = send_once();
    let (message, drops) = TestDrop::new_pair();
    sender.send_nowait(message).unwrap();
    assert_eq!(drops.count(), 0);
    //assert!(block_on(receiver).is_ok());
    drop(receiver);
    assert_eq!(drops.count(), 1);
}

#[test]
fn send_once_receiver_dropped_complete() {
    let (sender, receiver) = send_once();
    let (message, drops) = TestDrop::new_pair();
    sender.send_nowait(message).unwrap();
    let result = block_on(receiver).unwrap();
    assert_eq!(drops.count(), 0);
    drop(result);
    assert_eq!(drops.count(), 1);
}

#[test]
fn send_once_receiver_stream_one() {
    let (sender, mut receiver) = send_once::<u32>();
    sender.send_nowait(5).unwrap();
    assert_eq!(receiver.is_terminated(), false);
    assert_eq!(block_on(receiver.next()), Some(5));
    assert_eq!(receiver.is_terminated(), true);
    assert_eq!(block_on(receiver.next()), None);
    assert_eq!(receiver.is_terminated(), true);
}

#[test]
fn send_once_receiver_stream_empty() {
    let (sender, mut receiver) = send_once::<u32>();
    drop(sender);
    assert_eq!(receiver.is_terminated(), true);
    assert_eq!(block_on(receiver.next()), None);
    assert_eq!(receiver.is_terminated(), true);
}

#[test]
fn send_once_receiver_cancel_early() {
    let (sender, receiver) = send_once::<u32>();
    assert_eq!(receiver.cancel(), None);
    assert!(sender.send_nowait(5).is_err());
}

#[test]
fn send_once_receiver_cancel_late() {
    let (sender, receiver) = send_once::<u32>();
    sender.send_nowait(5).unwrap();
    assert_eq!(receiver.cancel(), Some(5));
}

#[test]
fn send_once_flush() {
    let (sender, receiver) = send_once();
    let (message, drops) = TestDrop::new_pair();
    let waker = Arc::new(TestWaker::new());
    let wr = waker_ref(&waker);
    let mut cx = Context::from_waker(&wr);
    let mut flush = sender.send(message);
    assert_eq!(flush.is_terminated(), false);
    assert_eq!(Pin::new(&mut flush).poll(&mut cx), Poll::Pending);
    assert_eq!(flush.is_terminated(), false);
    assert_eq!(waker.count(), 0);
    assert_eq!(drops.count(), 0);

    let result = block_on(receiver).unwrap();
    assert_eq!(drops.count(), 0);
    assert_eq!(waker.count(), 1);
    assert_eq!(flush.is_terminated(), false);
    assert_eq!(Pin::new(&mut flush).poll(&mut cx), Poll::Ready(Ok(())));
    assert_eq!(flush.is_terminated(), true);
    drop(flush);
    assert_eq!(drops.count(), 0);
    drop(result);
    assert_eq!(drops.count(), 1);
}

#[test]
fn send_once_flush_drop() {
    let (sender, receiver) = send_once();
    let (message, drops) = TestDrop::new_pair();
    let waker = Arc::new(TestWaker::new());
    let wr = waker_ref(&waker);
    let mut cx = Context::from_waker(&wr);
    let mut flush = sender.send(message);
    assert_eq!(flush.is_terminated(), false);
    assert_eq!(Pin::new(&mut flush).poll(&mut cx), Poll::Pending);
    assert_eq!(flush.is_terminated(), false);
    assert_eq!(waker.count(), 0);
    assert_eq!(drops.count(), 0);

    drop(receiver);
    assert_eq!(drops.count(), 0);
    assert_eq!(waker.count(), 1);
    assert_eq!(flush.is_terminated(), false);
    assert_eq!(
        Pin::new(&mut flush).poll(&mut cx).map_err(|_| ()),
        Poll::Ready(Err(()))
    );
    assert_eq!(drops.count(), 1);
    assert_eq!(flush.is_terminated(), true);
    drop(flush);
    assert_eq!(drops.count(), 1);
}
