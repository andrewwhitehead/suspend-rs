use std::{thread, time::Duration};

use suspend_core::scoped::{with_scope, PinShared};

use self::utils::Track;

mod utils;

#[test]
fn pin_shared_unused() {
    let mut shared = PinShared::new(());
    shared.with(|_| {});
    assert_eq!(shared.into_inner(), ());
}

#[test]
fn pin_shared_borrow() {
    let mut shared = PinShared::new(());
    shared.with(|scope| {
        drop(scope.clone());
    });
    assert_eq!(shared.into_inner(), ());
}

#[cfg(feature = "std")]
#[test]
fn pin_shared_threaded() {
    // test that dropping the future after the task is queued
    // functions as expected
    let (track, effect) = Track::new_pair();
    let mut shared = PinShared::new(());
    shared.with(|scope| {
        track.call();
        thread::spawn({
            let scope = scope.clone();
            let track = track.clone();
            move || {
                track.call();
                drop(scope);
            }
        })
        .join()
        .unwrap();
        track.call();
    });
    // the third call should not be executed because the future was dropped
    assert_eq!(effect.call_count(), 3);
    assert_eq!(effect.drop_count(), 1);
    assert_eq!(shared.into_inner(), ());
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

#[cfg(feature = "std")]
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

#[cfg(feature = "std")]
#[test]
fn with_scope_delayed() {
    let (track, effect) = Track::new_pair();
    with_scope(|scope| {
        thread::spawn({
            move || {
                track.call();
                thread::sleep(Duration::new(1, 0));
                drop(scope);
                track.call();
            }
        });
    });
    assert_eq!(effect.call_count(), 2);
}
