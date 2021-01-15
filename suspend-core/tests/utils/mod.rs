use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::task::{Context, Poll, Waker};

#[derive(Default, Debug)]
pub struct State {
    calls: AtomicUsize,
    drops: AtomicUsize,
}

impl State {
    #[inline]
    pub fn call(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }

    #[inline]
    pub fn dropped(&self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }

    #[inline]
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    #[inline]
    pub fn drop_count(&self) -> usize {
        self.drops.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub struct Track(Arc<State>);

impl Track {
    // pub fn new() -> Self {
    //     Self(Arc::new(State::default()))
    // }

    pub fn new_pair() -> (Self, Effect) {
        let state = Arc::new(State::default());
        (Self(state.clone()), Effect(state))
    }
}

impl Clone for Track {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Deref for Track {
    type Target = State;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for Track {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(&*self.0, &*other.0)
    }
}

impl Eq for Track {}

impl Drop for Track {
    fn drop(&mut self) {
        self.0.dropped();
    }
}

pub struct Effect(Arc<State>);

impl Deref for Effect {
    type Target = State;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub fn notify_pair() -> (Notify, Wait) {
    let inner = Arc::new(NotifyInner {
        state: Mutex::new((None, false)),
    });
    (
        Notify {
            inner: inner.clone(),
        },
        Wait { inner },
    )
}

pub struct Notify {
    inner: Arc<NotifyInner>,
}

impl Notify {
    pub fn notify(self) {
        let mut guard = self.inner.state.lock().unwrap();
        if !guard.1 {
            guard.1 = true;
            if let Some(waker) = guard.0.take() {
                waker.wake();
            }
        }
    }
}

pub struct Wait {
    inner: Arc<NotifyInner>,
}

impl Future for Wait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut guard = self.inner.state.lock().unwrap();
        if guard.1 {
            Poll::Ready(())
        } else {
            guard.0.replace(cx.waker().clone());
            Poll::Pending
        }
    }
}

struct NotifyInner {
    // use an AtomicWaker if you want to do this normally
    state: Mutex<(Option<Waker>, bool)>,
}
