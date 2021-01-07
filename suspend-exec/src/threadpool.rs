use core::{
    any::Any,
    future::Future,
    mem,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
    time::Duration,
};
use std::{
    collections::VecDeque,
    marker::PhantomData,
    panic,
    sync::{Condvar, Mutex, MutexGuard},
    thread,
};

use suspend_channel::{send_once, ReceiveOnce, RecvError};
use suspend_core::shared::{PinShared, Ref, ScopedRef, Shared, SharedRef};
use tracing::info;

static DEFAULT_THREAD_NAME: &'static str = "threadpool";

type Runnable<'s> = Box<(dyn FnOnce() -> bool + Send + panic::UnwindSafe + 's)>;

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
            min_count: num_cpus::get() as _,
            max_count: None,
            thread_name: None,
        }
    }
}

pub struct ThreadPool {
    inner: Shared<ThreadPoolInner>,
}

impl ThreadPool {
    pub fn new(config: ThreadPoolConfig) -> Self {
        let slf = Self {
            inner: Shared::new(ThreadPoolInner {
                state: Mutex::new(ThreadPoolState {
                    idle_count: 0,
                    panic_count: 0,
                    thread_count: 0,
                    queue: VecDeque::new(),
                    shutdown: false,
                }),
                queue_cvar: Condvar::new(),
                idle_timeout: config.idle_timeout,
                thread_min_count: config.min_count,
                thread_max_count: config.max_count,
                thread_index: AtomicUsize::new(0),
                thread_name: config.thread_name,
            }),
        };
        if config.min_count > 0 {
            let mut state = slf.inner.state.lock().unwrap();
            for _ in 0..(config.min_count) {
                info!("pre-start");
                state.idle_count += 1;
                state.thread_count += 1;
                slf.inner
                    .build_thread()
                    .spawn({
                        let inner = slf.inner.borrow();
                        move || ThreadPoolInner::run_thread(inner)
                    })
                    .unwrap();
            }
        }
        slf
    }

    // TODO: add join
    // TODO: set stack size

    pub fn run<F, T>(&self, f: F) -> JoinTask<'static, T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (runnable, join) = JoinTask::new_pair(f);
        ThreadPoolInner::run_boxed(self.inner.scoped_ref(), runnable);
        join
    }

    // FIXME - ensure that any panic is returned to this caller,
    // not another call to the pool. Need to create a separate
    // completion object?
    pub fn scoped<'s, F, T>(&self, f: F) -> T
    where
        F: FnOnce(Scope<'s>) -> T + Send + 's,
        T: Send + 's,
    {
        let mut share = PinShared::new(self.inner.borrow());
        share.with(|s| f(Scope::new(s)))
    }

    pub async fn async_scoped<'s, F, T>(&'s self, f: F) -> thread::Result<T>
    where
        F: FnOnce(Scope<'s>) -> T + Send + 's,
        T: Send + 's,
    {
        let mut share = PinShared::new(self.inner.borrow());
        share
            .async_with(|s| {
                let (runnable, join) = JoinTask::new_pair({
                    let s = s.clone();
                    move || f(Scope::new(s))
                });
                ThreadPoolInner::run_boxed(s.scoped_ref(), unsafe { mem::transmute(runnable) });
                join
            })
            .await
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

        // change pool status and notify any waiting threads
        let mut state = self
            .inner
            .state
            .lock()
            .expect("Error locking threadpool mutex");
        info!("drop inner");
        state.shutdown = true;
        drop(state);

        // notify any waiting threads to discover shutdown state
        self.inner.queue_cvar.notify_all();

        // block until all references have been dropped (all threads have exited)
        if !self.inner.collect(Duration::from_secs(1)).unwrap() {
            panic!("timed out {}", self.inner.borrow_count());
        }

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
    idle_timeout: Option<Duration>,
    thread_min_count: usize,
    thread_max_count: Option<usize>,
    thread_index: AtomicUsize,
    thread_name: Option<String>,
}

impl ThreadPoolInner {
    fn build_thread(&self) -> thread::Builder {
        let base_name = self
            .thread_name
            .as_ref()
            .map(String::as_str)
            .unwrap_or(DEFAULT_THREAD_NAME);
        let ti = self.thread_index.fetch_add(1, Ordering::Relaxed);
        let name = format!("{}-{}", base_name, ti);
        thread::Builder::new().name(name)
    }

    fn run_boxed(inner: Ref<'_, Self>, f: Runnable<'static>) {
        info!("lock");
        let mut state = inner.state.lock().unwrap();
        state.queue.push_back(f);
        info!("queue");
        if !ThreadPoolInner::maybe_spawn(inner, state) {
            inner.queue_cvar.notify_one();
        }
    }

    fn run_thread(inner: SharedRef<Self>) {
        info!("start worker thread");
        let mut tasks = 0usize;
        let mut state = inner.state.lock().unwrap();

        loop {
            if state.shutdown {
                break;
            }

            if let Some(task) = state.queue.pop_front() {
                state.idle_count -= 1;
                Self::maybe_spawn(inner.scoped_ref(), state);

                info!("task");
                tasks += 1;
                let failed = task();
                info!("done task");

                state = inner.state.lock().unwrap();
                state.idle_count += 1;
                if failed {
                    // shut down the thread if the task panicked
                    info!("worker thread panicked");
                    state.panic_count += 1;
                    break;
                }
            } else if let Some(timeout) = inner.idle_timeout {
                let (guard, wait_result) = inner.queue_cvar.wait_timeout(state, timeout).unwrap();
                state = guard;
                if wait_result.timed_out() && state.thread_count > inner.thread_min_count {
                    info!("worker thread timed out");
                    break;
                }
            } else {
                state = inner.queue_cvar.wait(state).unwrap();
            }
        }

        state.idle_count -= 1;
        state.thread_count -= 1;
        drop(state);

        info!(
            "worker thread shut down after {} tasks, {}",
            tasks,
            thread::current().name().unwrap()
        );
    }

    fn maybe_spawn(inner: Ref<'_, Self>, mut state: MutexGuard<'_, ThreadPoolState>) -> bool {
        let idle_count = state.idle_count;
        let thread_count = state.thread_count;
        if idle_count == 0 || idle_count * 5 < state.queue.len() {
            if thread_count == 0
                || inner
                    .thread_max_count
                    .map(|max| max > thread_count)
                    .unwrap_or(true)
            {
                state.idle_count += 1;
                state.thread_count += 1;
                drop(state);

                let builder = inner.build_thread();
                inner.queue_cvar.notify_all();
                builder
                    .spawn({
                        let inner = inner.borrow();
                        move || Self::run_thread(inner)
                    })
                    .unwrap();
                return true;
            }
        }
        false
    }
}

struct ThreadPoolState {
    idle_count: usize,
    panic_count: usize,
    thread_count: usize,
    queue: VecDeque<Runnable<'static>>,
    shutdown: bool,
}

pub struct Scope<'s> {
    inner: ScopedRef<SharedRef<ThreadPoolInner>>,
    _marker: PhantomData<&'s mut &'s ()>,
}

impl<'s> Scope<'s> {
    #[inline]
    fn new(inner: ScopedRef<SharedRef<ThreadPoolInner>>) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }

    // TODO: method to fetch number of running threads for the scope?
    // method to await completion with a timeout?

    pub fn run<'t, F, T>(&'t self, f: F) -> JoinTask<'t, T>
    where
        F: FnOnce(Scope<'s>) -> T + Send + 's,
        T: Send + 's,
    {
        let (runnable, join) = JoinTask::new_pair({
            let scope = Scope::new(self.inner.clone());
            move || f(scope)
        });
        ThreadPoolInner::run_boxed(
            self.inner.scoped_ref(),
            // make the task 'static - we promise to collect any references
            unsafe { mem::transmute(runnable) },
        );
        join
    }
}

#[derive(Debug)]
pub struct JoinTask<'t, T> {
    handle: Option<thread::JoinHandle<()>>,
    result: ReceiveOnce<thread::Result<T>>,
    _marker: PhantomData<&'t ()>,
}

impl<'t, T> JoinTask<'t, T> {
    pub fn new_pair<F>(task: F) -> (Runnable<'t>, JoinTask<'t, T>)
    where
        F: FnOnce() -> T + Send + 't,
        T: Send + 't,
    {
        let (sender, result) = send_once();
        let runnable = Box::new(panic::AssertUnwindSafe(move || {
            let caught = panic::catch_unwind(panic::AssertUnwindSafe(task));
            let panicked = caught.is_err();
            sender.send_nowait(caught).ok();
            panicked
        }));
        let join = Self {
            handle: None,
            result,
            _marker: PhantomData,
        };
        (runnable, join)
    }

    pub fn join(mut self) -> thread::Result<T> {
        let result = if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
            self.result.try_recv().unwrap()
        } else {
            self.result.recv()
        };
        task_result(result)
    }

    // TODO: add join_timeout
}

impl<'t, T> Future for JoinTask<'t, T> {
    type Output = thread::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe { self.map_unchecked_mut(|join| &mut join.result) }
            .poll(cx)
            .map(task_result)
    }
}

#[inline]
fn task_result<T>(result: Result<thread::Result<T>, RecvError>) -> thread::Result<T> {
    match result {
        Ok(r) => r,
        Err(inc) => Err(Box::new(inc) as Box<dyn Any + Send + 'static>),
    }
}
