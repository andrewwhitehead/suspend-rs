use std::sync::{
    atomic::{
        AtomicBool, AtomicUsize,
        Ordering::{Relaxed, SeqCst},
    },
    Arc, Barrier, Condvar, Mutex,
};
use std::thread;
use std::time::Duration;

use futures_lite::{
    future::{block_on, poll_once},
    pin,
};

use suspend_exec::{ThreadPool, ThreadPoolConfig};

#[test]
fn thread_pool_unbounded() {
    tracing_subscriber::fmt::try_init().ok();

    let pool = Arc::new(ThreadPool::default());
    let count = 100;
    let cvar = Arc::new(Condvar::new());
    let mutex = Mutex::new(());
    let called = Arc::new(AtomicUsize::new(0));
    for _ in 0..count {
        pool.run({
            let cvar = cvar.clone();
            let called = called.clone();
            move || {
                thread::sleep(Duration::from_millis(5));
                called.fetch_add(1, SeqCst);
                cvar.notify_one();
            }
        });
    }
    let mut guard = mutex.lock().unwrap();
    loop {
        if called.load(SeqCst) == count {
            break;
        }
        guard = cvar.wait(guard).unwrap();
    }
}

#[test]
fn thread_pool_bounded() {
    tracing_subscriber::fmt::try_init().ok();

    let pool = Arc::new(ThreadPoolConfig::default().max_count(5).build());
    let count = 100;
    let cvar = Arc::new(Condvar::new());
    let mutex = Mutex::new(());
    let called = Arc::new(AtomicUsize::new(0));
    for _ in 0..count {
        pool.run({
            let cvar = cvar.clone();
            let called = called.clone();
            move || {
                thread::sleep(Duration::from_millis(5));
                called.fetch_add(1, SeqCst);
                cvar.notify_one();
            }
        });
    }
    let mut guard = mutex.lock().unwrap();
    loop {
        if called.load(SeqCst) == count {
            break;
        }
        guard = cvar.wait(guard).unwrap();
    }
}

#[test]
fn thread_pool_unblock() {
    tracing_subscriber::fmt::try_init().ok();

    let pool = Arc::new(ThreadPool::default());
    assert_eq!(
        block_on(pool.unblock(|| 99)).expect("Error unwrapping unblock result"),
        99
    );
}

#[test]
fn thread_pool_scoped() {
    tracing_subscriber::fmt::try_init().ok();

    let pool = Arc::new(ThreadPool::default());
    let result = AtomicBool::new(false);
    pool.scoped(|scope| {
        scope.run(|| {
            thread::sleep(Duration::from_millis(50));
            result.store(true, Relaxed);
        })
    });
    assert_eq!(result.load(Relaxed), true);
}

#[test]
fn async_scoped_drop() {
    tracing_subscriber::fmt::try_init().ok();

    // simply check that a never-polled async_scoped future does not block on drop
    let pool = Arc::new(ThreadPool::default());
    let fut = pool.async_scoped(|_scope| {});
    drop(fut);
}

#[test]
fn async_scoped_poll_and_drop() {
    tracing_subscriber::fmt::try_init().ok();

    let pool = Arc::new(ThreadPool::default());
    let barrier = Arc::new(Barrier::new(2));
    let called = Arc::new(AtomicBool::new(false));
    let fut = pool.async_scoped(|scope| {
        let barrier = Arc::clone(&barrier);
        let called = called.clone();
        scope.run(move || {
            barrier.wait();
            // FIXME - breaks in miri
            thread::sleep(Duration::from_millis(50));
            called.store(true, SeqCst);
        })
    });
    // poll once to queue the fn, then drop the future.
    // this should block until the closure completes
    {
        pin!(fut);
        assert_eq!(block_on(poll_once(&mut fut)), None);
        // ensure the function is actually executed. otherwise it
        // could be dropped without being run by the worker thread
        // (which is acceptable but not what is being tested)
        barrier.wait();
        // fut will now be dropped
    }
    assert_eq!(called.load(SeqCst), true);
}
