use criterion::{criterion_group, criterion_main, Criterion};

use suspend_channel::channel;
use suspend_core::listen::block_on;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn channel_many(c: &mut Criterion) {
    c.bench_function("suspend_core channel create", |b| {
        b.iter_batched_ref(
            || channel(),
            |(send, recv)| {
                let flush = send.send(1u32);
                assert_eq!(recv.wait_next(), Some(1));
                block_on(flush).unwrap();
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, channel_many);
criterion_main!(benches);
