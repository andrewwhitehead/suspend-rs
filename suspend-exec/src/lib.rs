use std::collections::VecDeque;
use std::marker::PhantomData;
use std::mem;
use std::panic;
use std::sync::Arc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use std::{
    any::Any,
    sync::atomic::{fence, AtomicUsize, Ordering},
};

use suspend_channel::{send_once, Incomplete, ReceiveOnce};
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
                queue_cvar: Condvar::new(),
                complete_cvar: Condvar::new(),
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

    #[inline]
    pub fn run<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.run_boxed(Box::new(f))
    }

    pub(crate) fn run_boxed(&self, f: Runnable<'static>) {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(panic) = state.panic.take() {
            panic::resume_unwind(panic);
        }
        state.queue.push_back(f);
        if !self.inner.maybe_spawn(state) {
            self.inner.queue_cvar.notify_one();
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

    pub fn scoped<'pool, 's, F, T>(&'pool self, f: F) -> T
    where
        F: FnOnce(&mut ThreadScope<'pool, 's>) -> T + Send + 's,
        T: Send + 'static,
    {
        let mut scope = ThreadScope {
            counter: AtomicUsize::new(0),
            pool: self,
            _marker: PhantomData,
        };
        f(&mut scope)
    }

    pub async fn async_scoped<'pool, 's, F, T>(&'pool self, f: F) -> Result<T, Incomplete>
    where
        F: FnOnce(&mut ThreadScope<'pool, 's>) -> T + Send + 's,
        T: Send + 'static,
    {
        let (sender, recv) = send_once();
        let mut scope = ThreadScope {
            counter: AtomicUsize::new(0),
            pool: self,
            _marker: PhantomData,
        };
        self.run_boxed(unsafe {
            mem::transmute(Box::new(|| {
                sender.send(f(&mut scope)).ok();
            }) as Runnable<'_>)
        });
        recv.await
    }
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
    queue_cvar: Condvar,
    complete_cvar: Condvar,
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
                    let (guard, wait_result) =
                        self.queue_cvar.wait_timeout(state, timeout).unwrap();
                    state = guard;
                    if wait_result.timed_out() && state.thread_count > state.thread_min_count {
                        info!("worker thread timed out");
                        self.complete_cvar.notify_all();
                        break;
                    }
                } else {
                    state = self.queue_cvar.wait(state).unwrap();
                }
            }
        }

        state.idle_count -= 1;
        state.thread_count -= 1;

        drop(state);
        // notify the main thread in case it is waiting for shutdown
        self.complete_cvar.notify_all();
        info!(
            "worker thread shut down after {} tasks, {}",
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
                self.queue_cvar.notify_one();
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
            info!("drop inner");
            state.shutdown = true;
            self.queue_cvar.notify_all();
            loop {
                if let Some(panic) = state.panic.take() {
                    drop(state); // no need to poison the mutex as well
                    panic::resume_unwind(panic);
                }
                if state.thread_count == 0 {
                    break;
                }
                if let Ok(guard) = self.complete_cvar.wait(state) {
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

pub struct ThreadScope<'pool, 's> {
    counter: AtomicUsize,
    pool: &'pool ThreadPool,
    _marker: PhantomData<&'s mut std::cell::Cell<()>>,
}

impl<'pool, 's> ThreadScope<'pool, 's> {
    pub fn run<F>(&mut self, f: F)
    where
        F: FnOnce() + Send + 's,
    {
        self.counter.fetch_add(1, Ordering::Relaxed);
        let sentinel = TrackScope {
            counter: &self.counter as *const AtomicUsize,
            cvar: &self.pool.inner.complete_cvar as *const Condvar,
        };
        self.pool.run_boxed(unsafe {
            mem::transmute(Box::new(|| {
                f();
                drop(sentinel);
            }) as Runnable<'s>)
        });
    }
}

impl<'pool, 's> Drop for ThreadScope<'pool, 's> {
    fn drop(&mut self) {
        let count = self.counter.load(Ordering::Relaxed);
        if count == 0 {
            fence(Ordering::Acquire);
        } else {
            let mut state = self.pool.inner.state.lock().unwrap();
            loop {
                if self.counter.load(Ordering::Acquire) == 0 {
                    break;
                }
                let guard = self.pool.inner.complete_cvar.wait(state).unwrap();
                state = guard;
            }
        }
    }
}

struct TrackScope {
    counter: *const AtomicUsize,
    cvar: *const Condvar,
}

unsafe impl Send for TrackScope {}

impl Drop for TrackScope {
    fn drop(&mut self) {
        unsafe {
            (&*self.counter).fetch_sub(1, Ordering::Release);
            (&*self.cvar).notify_all();
        }
    }
}
