use core::task::Poll;

#[cfg(feature = "std")]
use std::{panic, thread, time::Duration};

use suspend_channel::{task::task_fn, RecvError};
use suspend_core::thread::block_on;

use self::utils::TestDrop;

mod utils;

#[test]
fn task_fn_basic() {
    let (task, mut join) = task_fn(|| true);
    task.run();
    assert_eq!(join.try_join(), Poll::Ready(Ok(true)));
}

#[test]
fn task_fn_drop() {
    let (track, drops) = TestDrop::new_pair();
    let (task, mut join) = task_fn(|| {
        drop(track);
        true
    });
    assert_eq!(drops.count(), 0);
    drop(task);
    assert_eq!(drops.count(), 1);
    assert_eq!(join.try_join(), Poll::Ready(Err(RecvError::Incomplete)));
    assert_eq!(drops.count(), 1);
}

#[test]
fn task_fn_join_drop() {
    let (task, join) = task_fn(|| true);
    drop(join);
    task.run();
}

#[test]
fn task_fn_join_block_on() {
    let (task, join) = task_fn(|| true);
    task.run();
    assert_eq!(block_on(join), Ok(true));
}

#[cfg(all(feature = "std", not(miri)))]
#[test]
fn task_fn_join_timeout() {
    let (task, mut join) = task_fn(|| true);
    assert_eq!(
        join.join_timeout(Duration::from_millis(50)),
        Err(RecvError::TimedOut)
    );
    task.run();
    assert_eq!(join.join_timeout(Duration::from_millis(50)), Ok(true));
    assert_eq!(
        join.join_timeout(Duration::from_millis(50)),
        Err(RecvError::Terminated)
    );
}

#[test]
fn task_fn_both_drop() {
    task_fn(|| true);
}

#[test]
#[should_panic(expected = "expected")]
fn task_fn_panic() {
    let (task, _join) = task_fn(move || {
        panic!("expected");
    });
    task.run();
}

#[cfg(feature = "std")]
#[test]
fn task_fn_threaded() {
    let (task, join) = task_fn(move || true);
    let result = thread::spawn(move || task.run());
    assert_eq!(join.join(), Ok(true), "Task should complete successfully");
    result.join().expect("Error joining thread");
}

#[cfg(feature = "std")]
#[test]
fn task_fn_threaded_panic() {
    let (task, join) = task_fn(move || {
        panic!("expected");
    });
    let result = thread::spawn(|| task.run());
    assert_eq!(join.join(), Err(RecvError::Incomplete));
    result.join().expect_err("Expected a panic");
}
