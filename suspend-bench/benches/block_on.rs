use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

use futures_lite;
use suspend_core;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn listen_block_on_ready() {
    assert_eq!(
        suspend_core::listen::block_on(futures_lite::future::ready(true)),
        true
    )
}

fn futures_block_on_ready() {
    assert_eq!(
        futures_lite::future::block_on(futures_lite::future::ready(true)),
        true
    )
}

struct Repoll<T> {
    pending: usize,
    result: Option<T>,
    thread: bool,
}

impl<T> Repoll<T> {
    #[inline]
    pub fn new(value: T, pending: usize, thread: bool) -> Self {
        Repoll {
            pending,
            result: Some(value),
            thread,
        }
    }
}

impl<T> Unpin for Repoll<T> {}

impl<T> Future for Repoll<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<T> {
        let mut slf = self.get_mut();
        if slf.pending == 0 {
            Poll::Ready(slf.result.take().unwrap())
        } else {
            slf.pending -= 1;
            if slf.thread {
                let w = cx.waker().clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_nanos(1));
                    w.wake();
                });
            } else {
                cx.waker().wake_by_ref();
            }
            Poll::Pending
        }
    }
}

fn listen_block_on_repoll() {
    assert_eq!(
        suspend_core::listen::block_on(Repoll::new(true, 1, true)),
        true
    )
}

fn futures_block_on_repoll() {
    assert_eq!(
        futures_lite::future::block_on(Repoll::new(true, 1, true)),
        true
    )
}

fn bench_many(c: &mut Criterion) {
    c.bench_function("suspend_core block_on_ready", move |b| {
        b.iter(|| listen_block_on_ready());
    });
    c.bench_function("futures_lite block_on_ready", move |b| {
        b.iter(|| futures_block_on_ready());
    });
    c.bench_function("suspend_core block_on_repoll", move |b| {
        b.iter(|| listen_block_on_repoll());
    });
    c.bench_function("futures_lite block_on_repoll", move |b| {
        b.iter(|| futures_block_on_repoll());
    });
}

criterion_group!(benches, bench_many);
criterion_main!(benches);
