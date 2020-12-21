#![cfg_attr(not(feature = "std"), no_std)]

/// Data structure variants with run-time sanity checks for debugging
pub mod checked {
    use core::{
        cell::UnsafeCell,
        mem::MaybeUninit,
        ptr,
        sync::atomic::{AtomicBool, Ordering},
    };

    /// Equivalent to an `UnsafeCell<MaybeUninit<T>>`, this cell may hold uninitialized data.
    #[derive(Debug)]
    pub struct Maybe<T> {
        data: UnsafeCell<MaybeUninit<T>>,
        loaded: AtomicBool,
    }

    impl<T> Maybe<T> {
        /// Create a new, empty `Maybe<T>`.
        #[inline]
        pub const fn empty() -> Self {
            Self {
                data: UnsafeCell::new(MaybeUninit::uninit()),
                loaded: AtomicBool::new(false),
            }
        }

        /// Create a new, populated `Maybe<T>`.
        #[inline]
        pub const fn new(data: T) -> Self {
            Self {
                data: UnsafeCell::new(MaybeUninit::new(data)),
                loaded: AtomicBool::new(true),
            }
        }

        /// Access the contained value as a constant pointer.
        #[inline]
        pub unsafe fn as_ptr(&self) -> *const T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "as_ptr for uninitialized cell"
            );

            (&*self.data.get()).as_ptr()
        }

        /// Access the contained value as a mutable pointer.
        #[inline]
        pub unsafe fn as_mut_ptr(&mut self) -> *mut T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "as_mut_ptr for uninitialized cell"
            );

            (&mut *self.data.get()).as_mut_ptr()
        }

        /// Obtain a reference to the contained value. This method is `unsafe` because
        /// the value may not have been initialized.
        #[inline]
        pub unsafe fn as_ref(&self) -> &T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "as_ref for uninitialized cell"
            );

            &*(&*self.data.get()).as_ptr()
        }

        /// Obtain a mutable reference to the contained value.
        #[inline(always)]
        pub unsafe fn as_mut(&mut self) -> &mut T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "as_mut for uninitialized cell"
            );

            &mut *(&mut *self.data.get()).as_mut_ptr()
        }

        /// Override the `loaded` flag of the cell. This method is a no-op when using the
        /// unchecked implementation. It may be used when a value is inserted manually,
        /// for example by assigning to the dereferenced pointer.
        #[inline]
        pub fn set_loaded(&self, loaded: bool) {
            self.loaded.store(loaded, Ordering::Relaxed);
        }

        /// Drop the contained value in place.
        #[inline]
        pub unsafe fn clear(&self) {
            assert_eq!(
                self.loaded.swap(false, Ordering::Relaxed),
                true,
                "cleared uninitialized cell"
            );

            ptr::drop_in_place((&mut *self.data.get()).as_mut_ptr())
        }

        /// Load the contained value.
        #[inline]
        pub unsafe fn load(&self) -> T {
            assert_eq!(
                self.loaded.swap(false, Ordering::Relaxed),
                true,
                "duplicate load"
            );

            ptr::read(self.data.get()).assume_init()
        }

        /// Store a new value in an occupied cell.
        #[inline]
        pub unsafe fn replace(&self, value: T) -> T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "replace on empty store"
            );
            let result = ptr::read(self.data.get()).assume_init();
            self.data.get().write(MaybeUninit::new(value));
            result
        }

        /// Store a new value in an empty cell.
        #[inline]
        pub unsafe fn store(&self, value: T) {
            assert_eq!(
                self.loaded.swap(true, Ordering::Relaxed),
                false,
                "duplicate store"
            );

            self.data.get().write(MaybeUninit::new(value))
        }
    }

    impl<T> Drop for Maybe<T> {
        fn drop(&mut self) {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                false,
                "cell not cleared before drop"
            );
        }
    }

    impl<T> From<T> for Maybe<T> {
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }

    /// Equivalent to an `UnsafeCell<MaybeUninit<T>>`, this cell may hold uninitialized data.
    /// Unlike `Maybe<T>` this structure is suited for data types with no `Drop` implementation.
    #[derive(Debug)]
    pub struct MaybeCopy<T> {
        data: UnsafeCell<MaybeUninit<T>>,
        loaded: AtomicBool,
    }

    // FIXME require Copy when const_fn is stable
    impl<T> MaybeCopy<T> {
        /// Create a new, empty `MaybeCopy<T>`.
        #[inline]
        pub const fn empty() -> Self {
            Self {
                data: UnsafeCell::new(MaybeUninit::uninit()),
                loaded: AtomicBool::new(false),
            }
        }

        /// Create a new, populated `MaybeCopy<T>`.
        #[inline]
        pub const fn new(data: T) -> Self {
            Self {
                data: UnsafeCell::new(MaybeUninit::new(data)),
                loaded: AtomicBool::new(true),
            }
        }
    }

    impl<T: Copy> MaybeCopy<T> {
        /// Access the contained value as a constant pointer.
        #[inline]
        pub unsafe fn as_ptr(&self) -> *const T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "as_ptr for uninitialized cell"
            );

            (&*self.data.get()).as_ptr()
        }

        /// Access the contained value as a mutable pointer.
        #[inline]
        pub unsafe fn as_mut_ptr(&mut self) -> *mut T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "as_mut_ptr for uninitialized cell"
            );

            (&mut *self.data.get()).as_mut_ptr()
        }

        /// Obtain a reference to the contained value. This method is `unsafe` because
        /// the value may not have been initialized.
        #[inline]
        pub unsafe fn as_ref(&self) -> &T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "as_ref for uninitialized cell"
            );

            &*(&*self.data.get()).as_ptr()
        }

        /// Obtain a mutable reference to the contained value.
        #[inline(always)]
        pub unsafe fn as_mut(&mut self) -> &mut T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "as_mut for uninitialized cell"
            );

            &mut *(&mut *self.data.get()).as_mut_ptr()
        }

        /// Override the `loaded` flag of the cell. This method is a no-op when using the
        /// unchecked implementation. It may be used when a value is inserted manually,
        /// for example by assigning to the dereferenced pointer.
        #[inline]
        pub fn set_loaded(&self, loaded: bool) {
            self.loaded.store(loaded, Ordering::Relaxed);
        }

        /// Load the contained value.
        #[inline]
        pub unsafe fn load(&self) -> T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "load of uninitialized cell"
            );

            ptr::read(self.data.get()).assume_init()
        }

        /// Store a new value in an occupied cell.
        #[inline]
        pub unsafe fn replace(&self, value: T) -> T {
            assert_eq!(
                self.loaded.load(Ordering::Relaxed),
                true,
                "replace on empty store"
            );
            let result = ptr::read(self.data.get()).assume_init();
            self.data.get().write(MaybeUninit::new(value));
            result
        }

        /// Store a new value in the cell.
        #[inline]
        pub unsafe fn store(&self, value: T) {
            self.loaded.store(true, Ordering::Relaxed);

            self.data.get().write(MaybeUninit::new(value))
        }
    }

    impl<T: Copy> From<T> for MaybeCopy<T> {
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }
}

/// Data structure variants without run-time checks
pub mod unchecked {
    use core::{cell::UnsafeCell, mem::MaybeUninit, ptr};

    /// Equivalent to an `UnsafeCell<MaybeUninit<T>>`, this cell may hold uninitialized data.
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct Maybe<T> {
        data: UnsafeCell<MaybeUninit<T>>,
    }

    impl<T> Maybe<T> {
        /// Create a new, empty `Maybe<T>`.
        #[inline(always)]
        pub const fn empty() -> Self {
            Self {
                data: UnsafeCell::new(MaybeUninit::uninit()),
            }
        }

        /// Create a new, populated `Maybe<T>`.
        #[inline(always)]
        pub const fn new(data: T) -> Self {
            Self {
                data: UnsafeCell::new(MaybeUninit::new(data)),
            }
        }

        /// Access the contained value as a constant pointer.
        #[inline(always)]
        pub unsafe fn as_ptr(&self) -> *const T {
            (&*self.data.get()).as_ptr()
        }

        /// Access the contained value as a mutable pointer.
        #[inline(always)]
        pub unsafe fn as_mut_ptr(&mut self) -> *mut T {
            (&mut *self.data.get()).as_mut_ptr()
        }

        /// Obtain a reference to the contained value. This method is `unsafe` because
        /// the value may not have been initialized.
        #[inline(always)]
        pub unsafe fn as_ref(&self) -> &T {
            &*(&*self.data.get()).as_ptr()
        }

        /// Obtain a mutable reference to the contained value.
        #[inline(always)]
        pub unsafe fn as_mut(&mut self) -> &mut T {
            &mut *(&mut *self.data.get()).as_mut_ptr()
        }

        /// Override the `loaded` flag of the cell. This method is a no-op when using the
        /// unchecked implementation. It may be used when a value is inserted manually,
        /// for example by assigning to the dereferenced pointer.
        #[inline]
        pub fn set_loaded(&self, _loaded: bool) {}

        /// Drop the contained value in place.
        #[inline(always)]
        pub unsafe fn clear(&self) {
            ptr::drop_in_place((&mut *self.data.get()).as_mut_ptr())
        }

        /// Load the contained value.
        #[inline(always)]
        pub unsafe fn load(&self) -> T {
            ptr::read(self.data.get()).assume_init()
        }

        /// Store a new value in an occupied cell.
        #[inline(always)]
        pub unsafe fn replace(&self, value: T) -> T {
            let result = self.load();
            self.data.get().write(MaybeUninit::new(value));
            result
        }

        /// Store a new value in an empty cell.
        #[inline(always)]
        pub unsafe fn store(&self, value: T) {
            self.data.get().write(MaybeUninit::new(value))
        }
    }

    impl<T> From<T> for Maybe<T> {
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }

    /// Equivalent to an `UnsafeCell<MaybeUninit<T>>`, this cell may hold uninitialized data.
    /// Unlike `Maybe<T>` this structure is suited for data types with no `Drop` implementation.
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct MaybeCopy<T> {
        data: UnsafeCell<MaybeUninit<T>>,
    }

    // FIXME require Copy when const_fn is stable
    impl<T> MaybeCopy<T> {
        /// Create a new, empty `MaybeCopy<T>`.
        #[inline]
        pub const fn empty() -> Self {
            Self {
                data: UnsafeCell::new(MaybeUninit::uninit()),
            }
        }

        /// Create a new, populated `MaybeCopy<T>`.
        #[inline]
        pub const fn new(data: T) -> Self {
            Self {
                data: UnsafeCell::new(MaybeUninit::new(data)),
            }
        }
    }

    impl<T: Copy> MaybeCopy<T> {
        /// Access the contained value as a constant pointer.
        #[inline(always)]
        pub unsafe fn as_ptr(&self) -> *const T {
            (&*self.data.get()).as_ptr()
        }

        /// Access the contained value as a mutable pointer.
        #[inline(always)]
        pub unsafe fn as_mut_ptr(&mut self) -> *mut T {
            (&mut *self.data.get()).as_mut_ptr()
        }

        /// Obtain a reference to the contained value. This method is `unsafe` because
        /// the value may not have been initialized.
        #[inline(always)]
        pub unsafe fn as_ref(&self) -> &T {
            &*(&*self.data.get()).as_ptr()
        }

        /// Obtain a mutable reference to the contained value.
        #[inline(always)]
        pub unsafe fn as_mut(&mut self) -> &mut T {
            &mut *(&mut *self.data.get()).as_mut_ptr()
        }

        /// Override the `loaded` flag of the cell. This method is a no-op when using the
        /// unchecked implementation. It may be used when a value is inserted manually,
        /// for example by assigning to the dereferenced pointer.
        #[inline]
        pub fn set_loaded(&self, _loaded: bool) {}

        /// Load the contained value.
        #[inline(always)]
        pub unsafe fn load(&self) -> T {
            ptr::read(self.data.get()).assume_init()
        }

        /// Store a new value in an occupied cell.
        pub unsafe fn replace(&self, value: T) -> T {
            let result = self.load();
            self.data.get().write(MaybeUninit::new(value));
            result
        }

        /// Store a new value in an empty cell.
        #[inline(always)]
        pub unsafe fn store(&self, value: T) {
            self.data.get().write(MaybeUninit::new(value))
        }
    }

    impl<T: Copy> From<T> for MaybeCopy<T> {
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{checked, unchecked};

    #[test]
    fn checked_init_read_write() {
        unsafe {
            let cell = checked::Maybe::new(1);
            assert_eq!(cell.load(), 1);
            cell.store(2);
            assert_eq!(cell.load(), 2);
        }
    }

    #[test]
    fn unchecked_init_read_write() {
        unsafe {
            let cell = unchecked::Maybe::new(1);
            assert_eq!(cell.load(), 1);
            cell.store(2);
            assert_eq!(cell.load(), 2);
        }
    }
}
