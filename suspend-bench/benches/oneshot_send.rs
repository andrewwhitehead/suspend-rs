use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use futures_channel::oneshot as futures_oneshot;
use futures_lite::future::block_on;
use oneshot_rs as oneshot;
use suspend_channel::send_once;

#[cfg(target = "macos")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn send_once_many(count: usize) {
    for _ in 0..count {
        let (sender, receiver) = send_once();
        sender.send(1).unwrap();
        block_on(receiver).unwrap();
    }
}

fn futures_many(count: usize) {
    for _ in 0..count {
        let (sender, receiver) = futures_oneshot::channel();
        sender.send(1).unwrap();
        block_on(receiver).unwrap();
    }
}

fn oneshot_many(count: usize) {
    for _ in 0..count {
        let (sender, receiver) = oneshot::channel();
        sender.send(1).unwrap();
        block_on(receiver).unwrap();
    }
}

fn bench_many(c: &mut Criterion) {
    let count = 5000;
    c.bench_with_input(
        BenchmarkId::new("send_once-many", count),
        &count,
        |b, &s| {
            b.iter(|| send_once_many(s));
        },
    );
    c.bench_with_input(BenchmarkId::new("futures-many", count), &count, |b, &s| {
        b.iter(|| futures_many(s));
    });
    c.bench_with_input(BenchmarkId::new("oneshot-many", count), &count, |b, &s| {
        b.iter(|| oneshot_many(s));
    });
}

criterion_group!(benches, bench_many);
criterion_main!(benches);
