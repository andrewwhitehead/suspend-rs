use criterion::{criterion_group, criterion_main, Criterion};

use suspend_core;

#[cfg(target = "macos")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn scope_unused() {
    assert_eq!(suspend_core::shared::with_scope(|_scope| true), true)
}

fn scope_borrow() {
    assert_eq!(
        suspend_core::shared::with_scope(|scope| {
            drop(scope.clone());
            drop(scope.clone());
            true
        }),
        true
    )
}

fn bench_many(c: &mut Criterion) {
    c.bench_function(format!("suspend_core scope unused").as_str(), move |b| {
        b.iter(|| scope_unused());
    });
    c.bench_function(format!("suspend_core scope borrow").as_str(), move |b| {
        b.iter(|| scope_borrow());
    });
}

criterion_group!(benches, bench_many);
criterion_main!(benches);
