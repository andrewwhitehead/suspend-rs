use criterion::{criterion_group, criterion_main, Criterion};

use futures_channel::oneshot as futures_oneshot;
use oneshot_rs as oneshot;
use suspend_channel::send_once;
use suspend_core::listen::block_on;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn test_send_once_recv() {
    let (sender, receiver) = send_once();
    sender.send_nowait(1).unwrap();
    receiver.recv().unwrap();
}

fn test_send_once_block_on() {
    let (sender, receiver) = send_once();
    sender.send_nowait(1).unwrap();
    block_on(receiver).unwrap();
}

fn test_futures_oneshot() {
    let (sender, receiver) = futures_oneshot::channel();
    sender.send(1).unwrap();
    block_on(receiver).unwrap();
}

fn test_faern_oneshot() {
    let (sender, receiver) = oneshot::channel();
    sender.send(1).unwrap();
    block_on(receiver).unwrap();
}

fn bench_many(c: &mut Criterion) {
    c.bench_function("suspend_core send_once", move |b| {
        b.iter(|| test_send_once_recv());
    });
    c.bench_function("suspend_core send_once block_on", move |b| {
        b.iter(|| test_send_once_block_on());
    });
    c.bench_function("futures oneshot", move |b| {
        b.iter(|| test_futures_oneshot());
    });
    c.bench_function("faern oneshot", move |b| {
        b.iter(|| test_faern_oneshot());
    });
}

criterion_group!(benches, bench_many);
criterion_main!(benches);
