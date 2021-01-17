use core::time::Duration;
use std::task::Poll;
use std::thread;

use suspend_core::{
    pin,
    shared::{with_scope, Lender, PinShared},
};

use self::utils::{poll_once, Repoll, Track};

mod utils;

#[test]
fn lender_unshared_get_mut() {
    let mut lender = Lender::new(true);
    *lender.get_mut().expect("Error collecting lender") = false;
    assert_eq!(lender.to_owned(), false);
}

#[test]
fn lender_unshared_collect_poll() {
    let mut lender = Lender::new(true);
    assert_eq!(poll_once(lender.collect()).is_ready(), true);
}

#[test]
fn lender_unshared_collect_wait() {
    let mut lender = Lender::new(true);
    assert_eq!(
        lender.collect().wait(Duration::from_millis(100)).is_ok(),
        true
    );
}

#[test]
fn lender_borrow_collect_pending() {
    let mut lender = Lender::new(true);
    let _borrow = lender.borrow();
    assert_eq!(poll_once(lender.collect()).is_pending(), true);
}

#[test]
fn lender_borrow_collect_wait_timeout() {
    let mut lender = Lender::new(true);
    let _borrow = lender.borrow();
    lender
        .collect()
        .wait(Duration::from_millis(100))
        .expect_err("Should time out");
}

#[test]
fn lender_borrow_get_mut() {
    let mut lender = Lender::new(true);
    for _ in 0..10 {
        drop(lender.borrow());
    }
    lender.get_mut().expect("Error collecting lender");
}

#[test]
fn lender_borrow_get_mut_fail() {
    let mut lender = Lender::new(true);
    let borrow = lender.borrow();
    assert_eq!(lender.get_mut().is_none(), true);
    drop(borrow);
    lender.get_mut().expect("Error collecting lender");
}

#[test]
fn lender_borrow_drop() {
    let lender = Lender::new(true);
    let borrow = lender.borrow();
    drop(lender);
    drop(borrow);
}

#[test]
fn lender_borrow_poll_drop() {
    let mut lender = Lender::new(true);
    let borrow = lender.borrow();
    assert_eq!(poll_once(lender.collect()), Poll::Pending);
    drop(lender);
    drop(borrow);
}

#[test]
fn lender_borrow_collect_threaded() {
    let mut lender = Lender::new(true);
    let threads = (0..10)
        .map(|_| {
            thread::spawn({
                let borrow = lender.borrow();
                move || {
                    drop(borrow);
                }
            })
        })
        .collect::<Vec<_>>();
    lender
        .collect()
        .wait(Duration::from_millis(1000))
        .expect("Error collecting lender");
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
    let mut scope = PinShared::new(());
    let fut = scope.async_with(|_scope| async move {
        track.call();
        let th = thread::spawn({
            move || {
                // FIXME - breaks in miri without isolation disabled
                thread::sleep(Duration::from_millis(500));
            }
        });
        track.call();
        // ensure we return Pending - this is the end of the first poll
        Repoll::new().await;
        th.join().unwrap();
        track.call();
    });
    // poll once to queue the fn, then drop the future.
    // this should block until the closure completes
    {
        pin!(fut);
        assert_eq!(poll_once(fut), Poll::Pending);
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
fn with_scope_unused() {
    assert_eq!(with_scope(|_scope| true), true);
}

#[test]
fn with_scope_borrow() {
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
fn with_scope_borrow_threaded() {
    assert_eq!(
        with_scope(|scope| {
            for _ in 0..10 {
                thread::spawn({
                    let scope = scope.clone();
                    move || {
                        drop(scope);
                    }
                })
                .join()
                .unwrap();
            }
            true
        }),
        true
    );
}
