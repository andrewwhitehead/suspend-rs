use criterion::{criterion_group, criterion_main, Criterion};

use blocking;
use once_cell::sync::Lazy;
use suspend_core::thread::block_on;
use suspend_exec::ThreadPool;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn thread_pool_run_join() {
    static POOL: Lazy<ThreadPool> = Lazy::new(ThreadPool::default);
    assert_eq!(POOL.run(move || true).join().unwrap(), true);
}

fn thread_pool_run_await() {
    static POOL: Lazy<ThreadPool> = Lazy::new(ThreadPool::default);
    assert_eq!(block_on(POOL.run(move || true)).unwrap(), true);
}

fn thread_pool_scoped() {
    static POOL: Lazy<ThreadPool> = Lazy::new(ThreadPool::default);
    assert_eq!(POOL.scoped(|_scope| true), true);
}

fn thread_pool_scoped_run() {
    static POOL: Lazy<ThreadPool> = Lazy::new(ThreadPool::default);
    POOL.scoped(|scope| {
        assert_eq!(scope.run(move |_| true).join().unwrap(), true);
    });
}

fn blocking_unblock() {
    assert_eq!(block_on(blocking::unblock(move || true)), true);
}

fn bench_many(c: &mut Criterion) {
    c.bench_function("thread_pool_run_join", |b| {
        b.iter(|| thread_pool_run_join());
    });
    c.bench_function("thread_pool_run_await", |b| {
        b.iter(|| thread_pool_run_await());
    });
    c.bench_function("thread_pool_scoped", |b| {
        b.iter(|| thread_pool_scoped());
    });
    c.bench_function("thread_pool_scoped_run", |b| {
        b.iter(|| thread_pool_scoped_run());
    });
    c.bench_function("blocking_unblock", |b| {
        b.iter(|| blocking_unblock());
    });
}

criterion_group!(benches, bench_many);
criterion_main!(benches);
