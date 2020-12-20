use std::any::Any;
use std::collections::VecDeque;
use std::panic;
use std::sync::Arc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use suspend_channel::{send_once, ReceiveOnce};
use tracing::info;

static DEFAULT_THREAD_NAME: &'static str = "threadpool";

type Runnable<'s> = Box<dyn FnOnce() + Send + 's>;

pub struct ThreadPoolConfig {
    idle_timeout: Option<Duration>,
    min_count: usize,
    max_count: Option<usize>,
    thread_name: Option<String>,
}

impl ThreadPoolConfig {
    pub fn idle_timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.idle_timeout = timeout.into();
        self
    }

    pub fn max_count(mut self, count: impl Into<Option<usize>>) -> Self {
        self.max_count = count.into();
        self
    }

    pub fn min_count(mut self, count: usize) -> Self {
        self.min_count = count;
        self
    }

    pub fn thread_name(mut self, name: impl Into<Option<String>>) -> Self {
        self.thread_name = name.into();
        self
    }

    pub fn build(self) -> ThreadPool {
        ThreadPool::new(self)
    }
}

impl Default for ThreadPoolConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Some(Duration::from_millis(500)),
            min_count: num_cpus::get(),
            max_count: None,
            thread_name: None,
        }
    }
}

pub struct ThreadPool {
    inner: Arc<ThreadPoolInner>,
}

impl ThreadPool {
    pub fn new(config: ThreadPoolConfig) -> Self {
        let slf = Self {
            inner: Arc::new(ThreadPoolInner {
                state: Mutex::new(ThreadPoolState {
                    counter: 0,
                    queue: VecDeque::new(),
                    idle_count: 0,
                    idle_timeout: config.idle_timeout,
                    thread_min_count: config.min_count,
                    thread_max_count: config.max_count,
                    thread_count: 0,
                    thread_name: config.thread_name,
                    panic: None,
                    shutdown: false,
                }),
                cvar: Condvar::new(),
            }),
        };
        if config.min_count > 0 {
            let mut state = slf.inner.state.lock().unwrap();
            for _ in 0..(config.min_count) {
                info!("pre-start");
                state
                    .build_thread()
                    .spawn({
                        let inner = slf.inner.clone();
                        move || inner.run_thread()
                    })
                    .unwrap();
            }
        }
        slf
    }

    // TODO: add join
    // TODO: set stack size

    pub fn run<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(panic) = state.panic.take() {
            panic::resume_unwind(panic);
        }
        state.queue.push_back(Box::new(f));
        if !self.inner.maybe_spawn(state) {
            self.inner.cvar.notify_one();
        }
    }

    pub fn unblock<F, T>(&self, f: F) -> ReceiveOnce<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (sender, recv) = send_once();
        self.run(|| {
            sender.send(f()).ok();
        });
        recv
    }

    // pub fn scope<'pool, 's, F, T>(&'pool self, f: F) -> T
    // where
    //     F: FnOnce(&mut ThreadScope<'pool, 's, T>) -> T + Send + 's,
    //     T: 'static,
    // {
    //     //
    // }

    // pub fn async_scope<'pool, 's, F, T>(&'pool self, f: F) -> ScopeFuture<'s, T>
    // where
    //     F: FnOnce(&mut ThreadScope<'pool, 's, T>) -> T + Send + 's,
    //     T: 'static,
    // {
    //     //
    // }
}

impl Default for ThreadPool {
    fn default() -> Self {
        ThreadPoolConfig::default().into()
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        if thread::panicking() {
            return;
        }
        self.inner.shutdown();
        info!("dropped pool");
    }
}

impl From<ThreadPoolConfig> for ThreadPool {
    fn from(config: ThreadPoolConfig) -> Self {
        ThreadPool::new(config)
    }
}

struct ThreadPoolInner {
    state: Mutex<ThreadPoolState>,
    cvar: Condvar,
}

impl ThreadPoolInner {
    fn run_thread(self: Arc<Self>) {
        info!("start worker thread");
        let mut state = self.state.lock().unwrap();
        let mut tasks = 0usize;
        let idle_timeout = state.idle_timeout;

        loop {
            if state.shutdown {
                break;
            }

            if let Some(task) = state.queue.pop_front() {
                state.idle_count -= 1;
                tasks += 1;
                self.maybe_spawn(state);
                info!("run task");
                let panic_result = panic::catch_unwind(panic::AssertUnwindSafe(task));
                state = self.state.lock().unwrap();
                state.idle_count += 1;
                if let Err(panic) = panic_result {
                    info!("worker thread panicked");
                    if state.panic.is_none() {
                        state.panic.replace(panic);
                        state.shutdown = true;
                    }
                    break;
                }
            } else {
                if let Some(timeout) = idle_timeout {
                    let (guard, wait_result) = self.cvar.wait_timeout(state, timeout).unwrap();
                    state = guard;
                    if wait_result.timed_out() && state.thread_count > state.thread_min_count {
                        info!("worker thread timed out");
                        break;
                    }
                } else {
                    state = self.cvar.wait(state).unwrap();
                }
            }
        }

        state.idle_count -= 1;
        state.thread_count -= 1;
        // notify the main thread in case it is waiting for shutdown
        self.cvar.notify_one();
        info!(
            "worker thread shut down: {} tasks, {}",
            tasks,
            thread::current().name().unwrap()
        );
    }

    fn maybe_spawn(self: &Arc<Self>, mut state: MutexGuard<ThreadPoolState>) -> bool {
        if state.idle_count == 0 || state.idle_count * 5 < state.queue.len() {
            if state
                .thread_max_count
                .map(|max| max > state.thread_count)
                .unwrap_or(true)
                || state.thread_count == 0
            {
                let builder = state.build_thread();
                drop(state); // in case another thread becomes idle first
                self.cvar.notify_one();
                builder
                    .spawn({
                        let inner = self.clone();
                        move || inner.run_thread()
                    })
                    .unwrap();
                return true;
            }
        }
        false
    }

    fn shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.shutdown = true;
            self.cvar.notify_all();
            loop {
                if let Some(panic) = state.panic.take() {
                    drop(state); // no need to poison the mutex as well
                    panic::resume_unwind(panic);
                }
                if state.thread_count == 0 {
                    break;
                }
                if let Ok(guard) = self.cvar.wait(state) {
                    state = guard;
                } else {
                    break;
                }
            }
        }
    }
}

struct ThreadPoolState {
    counter: usize,
    queue: VecDeque<Runnable<'static>>,
    idle_count: usize,
    idle_timeout: Option<Duration>,
    thread_min_count: usize,
    thread_max_count: Option<usize>,
    thread_count: usize,
    thread_name: Option<String>,
    panic: Option<Box<dyn Any + Send>>,
    shutdown: bool,
}

impl ThreadPoolState {
    fn build_thread(&mut self) -> thread::Builder {
        let base_name = self
            .thread_name
            .as_ref()
            .map(String::as_str)
            .unwrap_or(DEFAULT_THREAD_NAME);
        self.counter += 1;
        self.idle_count += 1;
        self.thread_count += 1;
        let name = format!("{}-{}", base_name, self.counter);
        thread::Builder::new().name(name)
    }
}

// struct ThreadScope<'pool, 's, T> {
//     pool: &'pool ThreadPool,
//     _marker: PhantomData<&'s T>,
// }

// impl<'pool, 's, T> ThreadScope<'pool, 's, T> {}

// struct ScopeFuture<'s, T> {
//     receiver: ReceiveOnce<T>,
//     _marker: PhantomData<&'s ()>,
// }

#[cfg(test)]
mod tests {
    use crate::ThreadPoolConfig;

    use super::ThreadPool;
    use futures_lite::future::block_on;
    use std::sync::{
        atomic::{AtomicUsize, Ordering::SeqCst},
        Arc, Condvar, Mutex,
    };
    use std::thread;
    use std::time::Duration;

    #[test]
    fn thread_pool_unbounded() {
        tracing_subscriber::fmt::try_init().ok();

        let pool = Arc::new(ThreadPool::default());
        let count = 100;
        let cvar = Arc::new(Condvar::new());
        let mutex = Mutex::new(());
        let called = Arc::new(AtomicUsize::new(0));
        for _ in 0..count {
            pool.run({
                let cvar = cvar.clone();
                let called = called.clone();
                move || {
                    thread::sleep(Duration::from_millis(5));
                    called.fetch_add(1, SeqCst);
                    cvar.notify_one();
                }
            });
        }
        let mut guard = mutex.lock().unwrap();
        loop {
            if called.load(SeqCst) == count {
                break;
            }
            guard = cvar.wait(guard).unwrap();
        }
    }

    #[test]
    fn thread_pool_bounded() {
        tracing_subscriber::fmt::try_init().ok();

        let pool = Arc::new(ThreadPoolConfig::default().max_count(5).build());
        let count = 100;
        let cvar = Arc::new(Condvar::new());
        let mutex = Mutex::new(());
        let called = Arc::new(AtomicUsize::new(0));
        for _ in 0..count {
            pool.run({
                let cvar = cvar.clone();
                let called = called.clone();
                move || {
                    thread::sleep(Duration::from_millis(5));
                    called.fetch_add(1, SeqCst);
                    cvar.notify_one();
                }
            });
        }
        let mut guard = mutex.lock().unwrap();
        loop {
            if called.load(SeqCst) == count {
                break;
            }
            guard = cvar.wait(guard).unwrap();
        }
    }

    #[test]
    fn thread_pool_unblock() {
        tracing_subscriber::fmt::try_init().ok();

        let pool = Arc::new(ThreadPool::default());
        assert_eq!(
            block_on(pool.unblock(|| 99)).expect("Error unwrapping unblock result"),
            99
        );
    }
}
