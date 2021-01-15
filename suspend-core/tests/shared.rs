use core::time::Duration;
use std::future::Future;
use std::task::Poll;
use std::thread;

use suspend_core::{
    pin,
    shared::{with_scope, PinShared, Shared},
};

#[cfg(feature = "std")]
use suspend_core::thread::block_on_poll;

use self::utils::{notify_pair, Track};

mod utils;

#[test]
fn shared_unshared() {
    let shared = Shared::new(true);
    assert_eq!(
        shared.collect_into().expect("Error collecting shared"),
        true
    );
}

#[test]
fn shared_collect_timeout() {
    let mut shared = Shared::new(true);
    let _borrow = shared.borrow();
    assert_eq!(
        shared
            .collect(Duration::from_millis(00))
            .expect("Error collecting shared"),
        false
    );
}

#[test]
fn shared_borrow_collect() {
    let mut shared = Shared::new(true);
    for _ in 0..10 {
        drop(shared.borrow());
    }
    assert_eq!(
        shared
            .collect(Duration::from_millis(500))
            .expect("Error collecting shared"),
        true
    );
}

#[test]
fn shared_borrow_collect_threaded() {
    let mut shared = Shared::new(true);
    let threads = (0..10)
        .map(|_| {
            thread::spawn({
                let borrow = shared.borrow();
                move || {
                    drop(borrow);
                }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        shared
            .collect(Duration::from_millis(500))
            .expect("Error collecting shared"),
        true
    );
    for th in threads {
        th.join().expect("Error joining thread");
    }
}

#[cfg(feature = "std")]
#[test]
fn pin_shared_async_poll_and_drop() {
    // test that dropping the future after the task is queued
    // functions as expected
    let (track, effect) = Track::new_pair();
    let (notify, wait) = notify_pair();
    let mut scope = PinShared::new(());
    let fut = scope.async_with(|_scope| async move {
        track.call();
        thread::spawn({
            move || {
                // FIXME - breaks in miri without isolation disabled
                thread::sleep(Duration::from_millis(500));
                notify.notify();
            }
        });
        track.call();
        wait.await;
        track.call();
    });
    // poll once to queue the fn, then drop the future.
    // this should block until the closure completes
    {
        pin!(fut);
        let mut fut = Some(fut);
        assert_eq!(
            block_on_poll(
                move |cx| {
                    assert!(fut.take().unwrap().poll(cx).is_pending());
                    Poll::Ready(true)
                },
                Duration::from_millis(100),
            ),
            Poll::Ready(true)
        );
        // should have spawned the thread and started waiting
        assert_eq!(effect.call_count(), 2);
        // fut will now be dropped
    }
    // the third call should not be executed because the future was dropped
    assert_eq!(effect.call_count(), 2);
    // track is owned by the future
    assert_eq!(effect.drop_count(), 1);
}

#[test]
fn shared_with_scope_unused() {
    assert_eq!(with_scope(|_scope| true), true);
}

#[test]
fn shared_with_scope_borrow() {
    assert_eq!(
        with_scope(|scope| {
            for _ in 0..10 {
                drop(scope.clone());
            }
            true
        }),
        true
    );
}

#[test]
fn shared_with_scope_borrow_threaded() {
    assert_eq!(
        with_scope(|scope| {
            thread::spawn(move || {
                drop(scope);
            });
            true
        }),
        true
    );
}
