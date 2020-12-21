use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

#[cfg(debug_assertions)]
pub use maybe_cell::checked::{Maybe, MaybeCopy};
#[cfg(not(debug_assertions))]
pub use maybe_cell::unchecked::{Maybe, MaybeCopy};

#[repr(transparent)]
pub struct BoxPtr<T: ?Sized>(NonNull<T>, PhantomData<Cell<T>>);

impl<T: ?Sized> BoxPtr<T> {
    #[inline]
    pub fn new(value: Box<T>) -> Self {
        unsafe { Self::new_unchecked(Box::into_raw(value)) }
        // unstable: Self(Box::into_raw_non_null(value))
    }

    #[inline]
    pub unsafe fn new_unchecked(ptr: *mut T) -> Self {
        debug_assert!(!ptr.is_null());
        Self(NonNull::new_unchecked(ptr), PhantomData)
    }

    // #[inline]
    // pub fn as_ptr(&self) -> *mut T {
    //     self.0.as_ptr()
    // }

    pub fn into_box(self) -> Box<T> {
        unsafe { Box::from_raw(self.0.as_ptr()) }
    }
}

impl<T> Clone for BoxPtr<T> {
    fn clone(&self) -> Self {
        Self(self.0, PhantomData)
    }
}

impl<T> Copy for BoxPtr<T> {}

impl<T> Deref for BoxPtr<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl<T> DerefMut for BoxPtr<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}
