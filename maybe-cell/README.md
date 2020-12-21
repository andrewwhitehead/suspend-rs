# maybe-cell

Use a `Maybe<T>` in place of an `UnsafeCell<MaybeUninit<T>>` for a friendlier API and optional error checking.

`MaybeCopy<T>` is provided for types that don't implement `Drop`.

This crate provides `checked` and `unchecked` variants to catch errors when working with the potentially-uninitialized cell. It is recommended to import the checked variant(s) based on a debug flag, for example:

```rust
#[cfg(debug_assertions)]
use maybe_cell::checked::Maybe;
#[cfg(not(debug_assertions))]
use maybe_cell::unchecked::Maybe;
```
