use criterion::{criterion_group, criterion_main, Criterion};

use parking;
use suspend_core;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn listen_create() {
    let _ = suspend_core::listen::Listener::new();
}

fn parking_create() {
    let _ = parking::Parker::new();
}

fn bench_many(c: &mut Criterion) {
    c.bench_function("listener create", move |b| {
        b.iter(|| listen_create());
    });
    c.bench_function("parking create", move |b| {
        b.iter(|| parking_create());
    });
}

criterion_group!(benches, bench_many);
criterion_main!(benches);
