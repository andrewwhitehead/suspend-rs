//! Utility data types

use core::{
    cell::Cell,
    fmt::{self, Debug, Formatter},
    marker::PhantomData,
    ops::Deref,
    ptr::NonNull,
};

/// A convenient wrapper around a boxed allocation allowing usage
/// as a shared reference
#[repr(transparent)]
pub struct BoxPtr<T: ?Sized>(NonNull<T>, PhantomData<Cell<T>>);

impl<T> BoxPtr<T> {
    /// Allocate a new `Box<T>` and convert it to a `BoxPtr<T>`
    #[inline]
    pub fn alloc(value: T) -> Self
    where
        T: Sized,
    {
        Self::new(Box::new(value))
    }
}

impl<T: ?Sized> BoxPtr<T> {
    /// Create a new `BoxPtr<T>` from a `Box<T>`
    #[inline]
    pub fn new(value: Box<T>) -> Self {
        unsafe { Self::new_unchecked(Box::into_raw(value)) }
        // unstable: Self(Box::into_raw_non_null(value))
    }

    /// Convert a mutable `T` pointer to a `BoxPtr<T>`
    pub const unsafe fn new_unchecked(ptr: *mut T) -> Self {
        Self(NonNull::new_unchecked(ptr), PhantomData)
    }

    /// Convert a `BoxPtr<T>` to a shared `T` pointer
    pub const fn as_ptr(&self) -> *const T {
        self.0.as_ptr()
    }

    /// Convert a shared `T` pointer to a `BoxPtr<T>`
    #[inline]
    pub unsafe fn from_ptr(ptr: *const T) -> Self {
        Self(
            NonNull::new(ptr as *mut T).expect("Expected non-zero pointer"),
            PhantomData,
        )
    }

    /// Unwrap the `Box<T>` pointed to by this `BoxPtr<T>`. The caller is responsible
    /// for ensuring that no additional references exist
    pub unsafe fn into_box(self) -> Box<T> {
        Box::from_raw(self.0.as_ptr())
    }
}

impl<T: ?Sized> Clone for BoxPtr<T> {
    fn clone(&self) -> Self {
        Self(self.0, PhantomData)
    }
}

impl<T: ?Sized> Copy for BoxPtr<T> {}

impl<T: ?Sized> Deref for BoxPtr<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl<T: Debug + ?Sized> Debug for BoxPtr<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("BoxPtr").field(&self.0).finish()
    }
}

unsafe impl<T: Send + ?Sized> Send for BoxPtr<T> {}
unsafe impl<T: Sync + ?Sized> Sync for BoxPtr<T> {}
