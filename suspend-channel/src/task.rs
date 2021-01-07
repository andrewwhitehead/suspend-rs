use core::{
    cell::UnsafeCell,
    future::Future,
    marker::PhantomData,
    mem::{forget, transmute},
    pin::Pin,
    ptr::NonNull,
    task::{Context, Poll},
};

use crate::{channel::Channel, RecvError};

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
        if slf.header.channel.drop_one_side(false) {
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

#[derive(Debug)]
pub struct JoinTask<'t, T> {
    task: Option<NonNull<TaskHeader<T>>>,
    _marker: PhantomData<&'t mut ()>,
}

impl<'t, T> JoinTask<'t, T> {
    pub fn join(mut self) -> TaskResult<T> {
        if let Some(task) = self.task.take() {
            let (result, dropped) = unsafe { &*task.as_ptr() }.channel.wait_read();
            if dropped {
                unsafe { ((&*task.as_ptr()).vtable.drop)(task.as_ptr() as *const ()) };
            }
            result.ok_or(RecvError::Incomplete)
        } else {
            Err(RecvError::Terminated)
        }
    }

    // TODO: add join_timeout
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

impl<T> Unpin for JoinTask<'_, T> {}

#[derive(Debug)]
pub struct TaskFn<'t> {
    task: NonNull<TaskVTable>,
    _marker: PhantomData<&'t mut ()>,
}

unsafe impl Send for TaskFn<'_> {}

impl TaskFn<'_> {
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

#[cfg(test)]
mod tests {
    use crate::RecvError;
    use suspend_core::listen::block_on;

    use super::task_fn;

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
    fn task_fn_both_drop() {
        task_fn(|| true);
    }
}
