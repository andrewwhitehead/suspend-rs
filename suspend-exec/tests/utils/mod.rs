use std::ops::Deref;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

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
    pub fn new() -> Self {
        Self(Arc::new(State::default()))
    }

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
