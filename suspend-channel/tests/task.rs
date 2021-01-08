use std::time::Duration;

use suspend_channel::{task::task_fn, RecvError};
use suspend_core::listen::block_on;

#[test]
fn task_fn_basic() {
    let (task, join) = task_fn(|| true);
    task.run();
    assert_eq!(join.join(), Ok(true));
}

#[test]
fn task_fn_drop() {
    let (task, join) = task_fn(|| true);
    drop(task);
    assert_eq!(join.join(), Err(RecvError::Incomplete));
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
