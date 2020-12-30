use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use blocking;
use futures_lite::future::block_on;
use suspend_exec::ThreadPool;

#[cfg(target = "macos")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn thread_pool_unblock(count: usize) {
    let pool = ThreadPool::default();
    for i in 0..count {
        block_on(pool.unblock(move || i)).unwrap();
    }
}

fn thread_pool_scoped(count: usize) {
    let pool = ThreadPool::default();
    for i in 0..count {
        pool.scoped(|scope| i);
    }
}

fn thread_pool_async_scoped(count: usize) {
    let pool = ThreadPool::default();
    for i in 0..count {
        block_on(pool.async_scoped(|scope| i));
    }
}

fn blocking_unblock(count: usize) {
    for i in 0..count {
        block_on(blocking::unblock(move || i));
    }
}

fn bench_many(c: &mut Criterion) {
    let count = 5000;
    c.bench_with_input(
        BenchmarkId::new("thread_pool_unblock", count),
        &count,
        |b, &s| {
            b.iter(|| thread_pool_unblock(s));
        },
    );
    c.bench_with_input(
        BenchmarkId::new("thread_pool_scoped", count),
        &count,
        |b, &s| {
            b.iter(|| thread_pool_scoped(s));
        },
    );
    c.bench_with_input(
        BenchmarkId::new("blocking_unblock", count),
        &count,
        |b, &s| {
            b.iter(|| blocking_unblock(s));
        },
    );
}

criterion_group!(benches, bench_many);
criterion_main!(benches);
