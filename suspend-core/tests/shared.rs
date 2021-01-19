use core::time::Duration;
use std::task::Poll;
use std::thread;

use suspend_core::shared::Lender;

use self::utils::poll_once;

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
fn lender_unshared_collect_into_poll() {
    let lender = Lender::new(true);
    assert_eq!(poll_once(lender.collect_into()), Poll::Ready(true));
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
fn lender_unshared_collect_into_wait() {
    let lender = Lender::new(true);
    assert_eq!(
        lender.collect_into().wait(Duration::from_millis(100)),
        Ok(true)
    );
}

#[test]
fn lender_borrow_collect_poll_retry() {
    let mut lender = Lender::new(true);
    let borrow = lender.borrow();
    let mut collect = lender.collect();
    assert_eq!(poll_once(&mut collect).is_pending(), true);
    drop(borrow);
    assert_eq!(poll_once(&mut collect).is_ready(), true);
}

#[test]
fn lender_borrow_collect_into_poll_retry() {
    let lender = Lender::new(true);
    let borrow = lender.borrow();
    let mut collect = lender.collect_into();
    assert_eq!(poll_once(&mut collect).is_pending(), true);
    drop(borrow);
    assert_eq!(poll_once(&mut collect), Poll::Ready(true));
}

#[test]
fn lender_borrow_collect_resolve_retry() {
    let mut lender = Lender::new(true);
    let borrow = lender.borrow();
    let lender = lender.collect().resolve().expect_err("Collect should fail");
    drop(borrow);
    assert_eq!(lender.collect().resolve().is_ok(), true);
}

#[test]
fn lender_borrow_collect_into_resolve_retry() {
    let lender = Lender::new(true);
    let borrow = lender.borrow();
    let lender = lender
        .collect_into()
        .resolve()
        .expect_err("Collect should fail");
    drop(borrow);
    assert_eq!(lender.collect_into().resolve(), Ok(true));
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
fn lender_borrow_collect_into_wait_timeout() {
    let lender = Lender::new(true);
    let _borrow = lender.borrow();
    lender
        .collect_into()
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
