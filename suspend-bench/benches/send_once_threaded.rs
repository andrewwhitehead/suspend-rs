use std::thread;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use futures_channel::oneshot as futures_oneshot;
use oneshot_rs as oneshot;
use suspend_channel::send_once;
use suspend_core::listen::block_on;
// use futures_lite::future::block_on;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn send_once_telephone(threads: usize) {
    let (sender, mut receiver) = send_once();
    for _ in 0..threads {
        let (next_send, next_receive) = send_once();
        thread::spawn(move || {
            let result = block_on(receiver).unwrap_or(0);
            next_send.send_nowait(result).unwrap();
        });
        receiver = next_receive;
    }
    let testval = 1001u32;
    sender.send_nowait(testval).unwrap();
    assert_eq!(block_on(receiver), Ok(testval));
}

fn futures_oneshot_telephone(threads: usize) {
    let (sender, mut receiver) = futures_oneshot::channel();
    for _ in 0..threads {
        let (next_send, next_receive) = futures_oneshot::channel();
        thread::spawn(move || {
            let result = block_on(receiver).unwrap_or(0);
            next_send.send(result).unwrap();
        });
        receiver = next_receive;
    }
    let testval = 1001u32;
    sender.send(testval).unwrap();
    assert_eq!(block_on(receiver), Ok(testval));
}

fn faern_oneshot_telephone(threads: usize) {
    let (sender, mut receiver) = oneshot::channel();
    for _ in 0..threads {
        let (next_send, next_receive) = oneshot::channel();
        thread::spawn(move || {
            let result = block_on(receiver).unwrap_or(0);
            next_send.send(result).unwrap();
        });
        receiver = next_receive;
    }
    let testval = 1001u32;
    sender.send(testval).unwrap();
    assert_eq!(receiver.recv(), Ok(testval));
}

fn bench_telephone(c: &mut Criterion) {
    let count = 1000;
    c.bench_with_input(
        BenchmarkId::new("send_once-telephone", count),
        &count,
        |b, &s| {
            b.iter(|| send_once_telephone(s));
        },
    );
    c.bench_with_input(
        BenchmarkId::new("futures-oneshot-telephone", count),
        &count,
        |b, &s| {
            b.iter(|| futures_oneshot_telephone(s));
        },
    );
    c.bench_with_input(
        BenchmarkId::new("faern-oneshot-telephone", count),
        &count,
        |b, &s| {
            b.iter(|| faern_oneshot_telephone(s));
        },
    );
}

criterion_group!(benches, bench_telephone);
criterion_main!(benches);
