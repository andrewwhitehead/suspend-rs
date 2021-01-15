//! Wrap a function and a channel in a single heap allocation, allowing
//! for async polling of the result.

extern crate alloc;

use alloc::boxed::Box;
use core::{
    cell::UnsafeCell,
    future::Future,
    marker::PhantomData,
    mem::{forget, transmute},
    pin::Pin,
    ptr::NonNull,
    task::{Context, Poll},
};

#[cfg(feature = "std")]
use core::mem::ManuallyDrop;
#[cfg(feature = "std")]
use suspend_core::Expiry;

use crate::{channel::Channel, RecvError};

/// The result type for a `JoinTask<'t, T>`
pub type TaskResult<T> = Result<T, RecvError>;

struct TaskVTable {
    invoke: unsafe fn(*const ()) -> bool,
    cancel: unsafe fn(*const ()),
    drop: unsafe fn(*const ()),
}

struct TaskHeader<T> {
    vtable: TaskVTable,
    channel: Channel<T>,
}

struct TaskInner<T, F> {
    header: TaskHeader<T>,
    body: UnsafeCell<Option<F>>,
}

impl<T, F> TaskInner<T, F> {
    fn alloc(self) -> NonNull<Self> {
        unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(self))) }
    }

    unsafe fn cancel_task(addr: *const ()) {
        let slf = &*(addr as *const Self);
        slf.body.get().drop_in_place();
        slf.body.get().write(None);
        if slf.header.channel.drop_one_side(true) {
            Self::drop_task(addr);
        }
    }

    unsafe fn drop_task(addr: *const ()) {
        drop(Box::from_raw(addr as *const Self as *mut Self));
    }
}

impl<T, F> TaskInner<T, F>
where
    F: FnOnce() -> T,
{
    pub fn new_fn(f: F) -> NonNull<Self> {
        Self::alloc(Self {
            header: TaskHeader {
                vtable: TaskVTable {
                    invoke: Self::invoke_fn,
                    cancel: Self::cancel_task,
                    drop: Self::drop_task,
                },
                channel: Channel::new(),
            },
            body: Some(f).into(),
        })
    }

    unsafe fn invoke_fn(addr: *const ()) -> bool
    where
        F: FnOnce() -> T,
    {
        let slf = &*(addr as *const Self);
        let task = (&mut *slf.body.get()).take().unwrap();
        let guard = TaskGuard(addr as *const TaskHeader<T>);
        let result = task();
        forget(guard);
        if slf.header.channel.write(result, None, true).is_err() {
            Self::drop_task(addr);
        }
        true
    }
}

struct TaskGuard<T>(*const TaskHeader<T>);

impl<T> Drop for TaskGuard<T> {
    fn drop(&mut self) {
        unsafe {
            let header = &*self.0;
            if header.channel.drop_one_side(true) {
                (header.vtable.drop)(self.0 as *const ());
            }
        }
    }
}

/// The result of a spawned `TaskFn` which can be polled or joined
#[derive(Debug)]
pub struct JoinTask<'t, T> {
    task: Option<NonNull<TaskHeader<T>>>,
    _marker: PhantomData<&'t mut ()>,
}

impl<'t, T> JoinTask<'t, T> {
    /// Block the current thread on the result of the task
    #[cfg(feature = "std")]
    pub fn join(self) -> TaskResult<T> {
        if let Some(task) = ManuallyDrop::new(self).task {
            let task = unsafe { &*task.as_ptr() };
            let (result, dropped) = task.channel.wait_read();
            if dropped || task.channel.drop_one_side(false) {
                unsafe { (task.vtable.drop)(task as *const TaskHeader<T> as *const ()) };
            }
            result.ok_or(RecvError::Incomplete)
        } else {
            Err(RecvError::Terminated)
        }
    }

    /// Block the current thread on the result of the task, with a timeout
    #[cfg(feature = "std")]
    pub fn join_timeout(&mut self, timeout: impl Into<Expiry>) -> TaskResult<T> {
        if let Some(task) = self.task.take() {
            let task_ref = unsafe { &*task.as_ptr() };
            let (result, dropped) = task_ref.channel.wait_read_timeout(timeout.into());
            if dropped {
                unsafe { (task_ref.vtable.drop)(task.as_ptr() as *const ()) };
                result.ok_or(RecvError::Incomplete)
            } else if let Some(result) = result {
                if task_ref.channel.drop_one_side(false) {
                    unsafe { (task_ref.vtable.drop)(task.as_ptr() as *const ()) };
                }
                Ok(result)
            } else {
                self.task.replace(task);
                Err(RecvError::TimedOut)
            }
        } else {
            Err(RecvError::Terminated)
        }
    }

    /// Try to fetch the result of the task
    pub fn try_join(&mut self) -> Poll<Result<T, RecvError>> {
        if let Some(task) = self.task.take() {
            let task_ref = unsafe { &*task.as_ptr() };
            if let Poll::Ready((result, dropped)) = task_ref.channel.read(None, false) {
                if dropped || task_ref.channel.drop_one_side(false) {
                    unsafe { (task_ref.vtable.drop)(task.as_ptr() as *const ()) };
                }
                Poll::Ready(result.ok_or(RecvError::Incomplete))
            } else {
                self.task.replace(task);
                Poll::Pending
            }
        } else {
            Poll::Ready(Err(RecvError::Terminated))
        }
    }
}

impl<'t, T> Future for JoinTask<'t, T> {
    type Output = TaskResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(addr) = self.task.take() {
            let task = unsafe { &*addr.as_ptr() };
            if let Poll::Ready((result, dropped)) = task.channel.read(Some(cx.waker()), false) {
                if dropped {
                    unsafe { (task.vtable.drop)(addr.as_ptr() as *const ()) };
                }
                Poll::Ready(result.ok_or(RecvError::Incomplete))
            } else {
                self.task.replace(addr);
                Poll::Pending
            }
        } else {
            Poll::Ready(Err(RecvError::Terminated))
        }
    }
}

impl<T> Drop for JoinTask<'_, T> {
    fn drop(&mut self) {
        unsafe {
            if let Some(task) = self.task {
                let slf = &*task.as_ptr();
                if slf.channel.drop_one_side(false) {
                    ((&*task.as_ptr()).vtable.drop)(task.as_ptr() as *const ());
                }
            }
        }
    }
}

unsafe impl<T: Send> Send for JoinTask<'_, T> {}

impl<T> Unpin for JoinTask<'_, T> {}

#[cfg(feature = "std")]
impl<T> ::std::panic::UnwindSafe for JoinTask<'_, T> {}
#[cfg(feature = "std")]
impl<T> ::std::panic::RefUnwindSafe for JoinTask<'_, T> {}

/// A handle for a heap-allocated task
#[derive(Debug)]
pub struct TaskFn<'t> {
    task: NonNull<TaskVTable>,
    _marker: PhantomData<&'t mut ()>,
}

unsafe impl Send for TaskFn<'_> {}

impl TaskFn<'_> {
    /// Execute the task. Panics are propagated to the caller of this method,
    /// and the paired `JoinTask` will receive `Err(RecvError::Incomplete)`.
    #[inline]
    pub fn run(self) {
        let addr = self.task.as_ptr();
        // a panic in the wrapped closure will automatically return
        // RecvError::Incomplete to the JoinTask and drop the allocation
        forget(self);
        unsafe { ((&*addr).invoke)(addr as *const ()) };
    }
}

impl Drop for TaskFn<'_> {
    fn drop(&mut self) {
        let addr = self.task.as_ptr();
        unsafe { ((&*addr).cancel)(addr as *const ()) };
    }
}

#[cfg(feature = "std")]
impl ::std::panic::UnwindSafe for TaskFn<'_> {}
#[cfg(feature = "std")]
impl ::std::panic::RefUnwindSafe for TaskFn<'_> {}

/// Create a new pair of a `TaskFn` and `JoinTask` from a coroutine
pub fn task_fn<'t, T: 't>(f: impl FnOnce() -> T + Send + 't) -> (TaskFn<'t>, JoinTask<'t, T>) {
    let inner = TaskInner::new_fn(f);
    let task = TaskFn {
        task: unsafe { transmute(inner) },
        _marker: PhantomData,
    };
    let join = JoinTask {
        task: unsafe { transmute(inner) },
        _marker: PhantomData,
    };
    (task, join)
}
