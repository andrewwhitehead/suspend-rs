use core::time::Duration;
use std::{sync::Arc, thread};

use suspend_core::{lock::Lock, LockError};

#[test]
fn lock_contend() {
    let lock = Lock::new(false);
    let _guard = lock.lock().expect("Error acquiring lock");
    assert_eq!(
        lock.try_lock().expect_err("Expected lock contention"),
        LockError::Contended
    );
}

#[test]
fn lock_contend_threaded() {
    let lock = Arc::new(Lock::new(false));
    let mut guard = lock.lock().expect("Error acquiring lock");
    let th = thread::spawn({
        let lock = Arc::clone(&lock);
        move || {
            let mut guard = lock.lock().expect("Error re-acquiring lock");
            assert_eq!(*guard, true);
            *guard = false;
        }
    });
    thread::sleep(Duration::from_millis(1));
    *guard = true;
    drop(guard);
    th.join().unwrap();
    let guard = lock.lock().expect("Error re-acquiring lock");
    assert_eq!(*guard, false);
}
