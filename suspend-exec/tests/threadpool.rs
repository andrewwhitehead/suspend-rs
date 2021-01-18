use std::panic;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use suspend_core::thread::block_on;
use suspend_exec::{RecvError, ThreadPool, ThreadPoolConfig};

use self::utils::Track;

mod utils;

fn run_test<T>(test: impl FnOnce() -> T) -> T {
    tracing_subscriber::fmt::try_init().ok();
    test()
}

// fn assert_panics_with<T>(msg: &str, test: T) -> ()
// where
//     T: FnOnce() -> thread::Result<()> + panic::UnwindSafe,
// {
//     tracing_subscriber::fmt::try_init().ok();
//     let prev_hook = panic::take_hook();
//     panic::set_hook(Box::new(|_| {}));
//     let result = test();
//     panic::set_hook(prev_hook);
//     if let Err(err) = result {
//         if !err
//             .downcast_ref::<String>()
//             .map(|e| &**e)
//             .or_else(|| err.downcast_ref::<&'static str>().map(|e| *e))
//             .map(|e| e.contains(msg))
//             .unwrap_or(false)
//         {
//             panic::resume_unwind(err)
//         }
//     } else {
//         panic!("Expected panic '{}'", msg);
//     }
// }

#[test]
fn thread_pool_unbounded() {
    run_test(|| {
        let pool = ThreadPool::default();
        let count = 100;
        let cvar = Arc::new(Condvar::new());
        let mutex = Mutex::new(());
        let track = Track::new();
        for _ in 0..count {
            // FIXME collect results instead of using mutex/cvar
            pool.run({
                let cvar = cvar.clone();
                let track = track.clone();
                move || {
                    thread::sleep(Duration::from_millis(5));
                    track.call();
                    drop(track);
                    cvar.notify_one();
                }
            });
        }
        let mut guard = mutex.lock().unwrap();
        loop {
            if track.call_count() == count {
                break;
            }
            guard = cvar.wait(guard).unwrap();
        }
        assert_eq!(track.drop_count(), count);
    })
}

#[test]
fn thread_pool_bounded() {
    run_test(|| {
        let pool = ThreadPoolConfig::default().max_count(5).build();
        let count = 100;
        let cvar = Arc::new(Condvar::new());
        let mutex = Mutex::new(());
        let track = Track::new();
        for _ in 0..count {
            pool.run({
                let cvar = cvar.clone();
                let track = track.clone();
                move || {
                    thread::sleep(Duration::from_millis(5));
                    track.call();
                    drop(track);
                    cvar.notify_one();
                }
            });
        }
        let mut guard = mutex.lock().unwrap();
        loop {
            if track.call_count() == count {
                break;
            }
            guard = cvar.wait(guard).unwrap();
        }
        assert_eq!(track.drop_count(), count);
    })
}

#[test]
fn thread_pool_run_join() {
    run_test(|| {
        let pool = ThreadPool::default();
        assert_eq!(pool.run(move || { true }).join(), Ok(true))
    })
}

#[test]
fn thread_pool_panic_run() {
    run_test(|| {
        let pool = ThreadPool::default();
        assert_eq!(
            pool.run(move || {
                panic!("expected");
            })
            .join(),
            Err(RecvError::Incomplete)
        )
    })
}

#[test]
fn thread_pool_run_async() {
    run_test(|| {
        let pool = ThreadPool::default();
        for i in 0..100 {
            assert_eq!(
                block_on(pool.run(move || i)).expect("Error unwrapping run result"),
                i
            );
        }
    })
}

#[test]
fn thread_pool_scoped() {
    run_test(|| {
        let pool = ThreadPool::default();
        let (track, _) = Track::new_pair();
        pool.scoped(|scope| {
            scope.run(|s| {
                thread::sleep(Duration::from_millis(50));
                track.call();
                s.run(|_| track.call());
            });
        });
        assert_eq!(track.call_count(), 2);
        assert_eq!(track.drop_count(), 0);
    })
}

#[test]
fn thread_pool_panic_scoped() {
    run_test(|| {
        let pool = ThreadPool::default();
        // all threads joined as scoped() ends
        assert_eq!(
            pool.scoped(|scope| {
                scope
                    .run(|_| {
                        panic!("expected");
                    })
                    .join()
            }),
            Err(RecvError::Incomplete)
        );
    })
}

// #[test]
// fn test_many() {
//     run_test(|| loop {
//         let pool = ThreadPool::default();
//         for i in 0..10 {
//             assert_eq!(pool.scoped(|_scope| i), i);
//         }
//         tracing::info!(".");
//     })
// }
