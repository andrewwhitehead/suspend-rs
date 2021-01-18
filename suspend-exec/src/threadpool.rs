use core::{
    any::Any,
    marker::PhantomData,
    mem,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use std::{
    collections::VecDeque,
    panic,
    sync::{Condvar, Mutex, MutexGuard},
    thread,
};

use suspend_channel::task::{task_fn, JoinTask, TaskFn};
use suspend_core::shared::{Lender, PinShared, ScopedRef, Shared, SharedRef};
use tracing::info;

static DEFAULT_THREAD_NAME: &'static str = "threadpool";

// TODO: set stack size
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
    inner: Lender<ThreadPoolInner>,
}

impl ThreadPool {
    pub fn new(config: ThreadPoolConfig) -> Self {
        let slf = Self {
            inner: Lender::new(ThreadPoolInner {
                state: Mutex::new(ThreadPoolState {
                    idle_count: 0,
                    panic_count: 0,
                    thread_count: 0,
                    queue: VecDeque::new(),
                    shutdown: false,
                }),
                state_cvar: Condvar::new(),
                idle_timeout: config.idle_timeout,
                thread_min_count: config.min_count,
                thread_max_count: config.max_count,
                thread_index: AtomicUsize::new(0),
                thread_name: config.thread_name,
            }),
        };
        if config.min_count > 0 {
            let mut state = slf.inner.as_ref().state.lock().unwrap();
            for _ in 0..(config.min_count) {
                info!("pre-start");
                state.idle_count += 1;
                state.thread_count += 1;
                slf.inner
                    .as_ref()
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

    pub fn run<F, T>(&self, f: F) -> JoinTask<'static, T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (task, join) = task_fn(f);
        ThreadPoolInner::run_boxed(self.inner.borrow_ref(), task);
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
        let inner = self.inner.as_ref();

        // change pool status and notify any waiting threads
        let mut state = inner.state.lock().expect("Error locking threadpool mutex");

        info!("drop pool inner");
        state.shutdown = true;
        drop(state);

        // notify any waiting threads to discover shutdown state
        inner.state_cvar.notify_all();

        // block until all references have been dropped (all threads have exited)
        self.inner.collect().wait(None).unwrap();

        info!("dropped pool");
    }
}

impl From<ThreadPoolConfig> for ThreadPool {
    fn from(config: ThreadPoolConfig) -> Self {
        ThreadPool::new(config)
    }
}

#[derive(Debug)]
struct ThreadPoolInner {
    state: Mutex<ThreadPoolState>,
    state_cvar: Condvar,
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

    fn run_boxed(inner: SharedRef<'_, Self>, task: TaskFn<'static>) {
        let mut state = inner.state.lock().unwrap();
        state.queue.push_back(task);
        if !ThreadPoolInner::maybe_spawn(inner, state) {
            inner.state_cvar.notify_one();
        }
    }

    fn run_thread(inner: Shared<Self>) {
        info!("start worker thread");
        let mut tasks = 0usize;
        let mut state = inner.state.lock().unwrap();
        let mut panic_err: Option<Box<dyn Any + Send + 'static>> = None;

        loop {
            if state.shutdown {
                break;
            }

            if let Some(task) = state.queue.pop_front() {
                state.idle_count -= 1;
                Self::maybe_spawn(inner.borrow_ref(), state);

                tasks += 1;
                if let Err(e) = panic::catch_unwind(panic::AssertUnwindSafe(|| task.run())) {
                    panic_err.replace(e);
                }

                state = inner.state.lock().unwrap();
                state.idle_count += 1;
                if panic_err.is_some() {
                    // shut down the thread if the task panicked
                    info!("worker thread panicked");
                    state.panic_count += 1;
                    break;
                }
            } else if let Some(timeout) = inner.idle_timeout {
                let (guard, timeout_result) =
                    inner.state_cvar.wait_timeout(state, timeout).unwrap();
                state = guard;
                if timeout_result.timed_out() && state.thread_count > inner.thread_min_count {
                    info!("worker thread timed out");
                    break;
                }
            } else {
                state = inner.state_cvar.wait(state).unwrap();
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

        if let Some(err) = panic_err {
            panic::resume_unwind(err);
        }
    }

    fn maybe_spawn(inner: SharedRef<'_, Self>, mut state: MutexGuard<'_, ThreadPoolState>) -> bool {
        let idle_count = state.idle_count;
        if idle_count == 0 || idle_count * 5 < state.queue.len() {
            let thread_count = state.thread_count;
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
                inner.state_cvar.notify_all();
                builder
                    .spawn({
                        let inner = inner.into_shared();
                        move || Self::run_thread(inner)
                    })
                    .unwrap();
                return true;
            }
        }
        false
    }
}

#[derive(Debug)]
struct ThreadPoolState {
    idle_count: usize,
    panic_count: usize,
    thread_count: usize,
    queue: VecDeque<TaskFn<'static>>,
    shutdown: bool,
}

#[derive(Debug)]
pub struct Scope<'s> {
    inner: ScopedRef<Shared<ThreadPoolInner>>,
    _marker: PhantomData<&'s mut &'s ()>,
}

impl<'s> Scope<'s> {
    #[inline]
    fn new(inner: ScopedRef<Shared<ThreadPoolInner>>) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }

    pub fn pending_count(&self) -> usize {
        self.inner.borrow_count()
    }

    pub fn run<'t, F, T>(&'t self, f: F) -> JoinTask<'t, T>
    where
        F: FnOnce(Scope<'s>) -> T + Send + 's,
        T: Send + 's,
    {
        let (runnable, join) = task_fn({
            let scope = Scope::new(self.inner.clone());
            move || f(scope)
        });
        ThreadPoolInner::run_boxed(
            self.inner.borrow_ref(),
            // make the task 'static - we promise to collect any references
            unsafe { mem::transmute(runnable) },
        );
        join
    }
}
