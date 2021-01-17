use criterion::{criterion_group, criterion_main, Criterion};

use futures_channel::mpsc::channel as mpsc_channel;
use suspend_channel::{channel, StreamIterExt};
use suspend_core::thread::block_on;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn channel_send(c: &mut Criterion) {
    c.bench_function("suspend_core channel send", |b| {
        b.iter_batched_ref(
            || channel(),
            |(send, recv)| {
                assert_eq!(send.try_send(1u32), Ok(()));
                assert_eq!(block_on(recv.stream_next()), Some(1));
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn mpsc_send(c: &mut Criterion) {
    c.bench_function("futures mpsc send", |b| {
        b.iter_batched_ref(
            || mpsc_channel(1),
            |(send, recv)| {
                assert_eq!(send.try_send(1u32), Ok(()));
                assert_eq!(block_on(recv.stream_next()), Some(1));
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, channel_send, mpsc_send);
criterion_main!(benches);
