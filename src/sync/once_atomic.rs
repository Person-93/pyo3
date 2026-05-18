// TODO stop allowing dead code in once_atomic
#![allow(dead_code, reason = "draft PR with items not used yet")]

use crate::prelude::{Py, PyAnyMethods};
use crate::{Borrowed, Bound, PyResult, PyTypeCheck, Python};
use core::convert::Infallible;
use core::marker::PhantomData;
use core::mem;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, Ordering};

#[repr(transparent)]
pub struct PyOnceAtomic<T> {
    inner: AtomicPtr<pyo3_ffi::PyObject>,
    _marker: PhantomData<T>,
}

impl<T> Default for PyOnceAtomic<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> PyOnceAtomic<T> {
    pub const fn new() -> Self {
        Self {
            inner: AtomicPtr::new(ptr::null_mut()),
            _marker: PhantomData,
        }
    }

    pub fn get<'a, 'py>(&'a self, py: Python<'py>) -> Option<Borrowed<'a, 'py, T>> {
        let p = self.inner.load(Ordering::SeqCst);
        unsafe { Borrowed::from_ptr_or_opt(py, p).map(|b| b.cast_unchecked()) }
    }

    pub fn get_or_init<'a, 'py>(
        &'a self,
        py: Python<'py>,
        f: impl FnOnce(Python<'py>) -> Bound<'py, T>,
    ) -> Borrowed<'a, 'py, T> {
        match self.get_or_try_init(py, move |py| -> Result<_, Infallible> { Ok(f(py)) }) {
            Ok(val) => val,
        }
    }

    pub fn get_or_try_init<'a, 'py, E>(
        &'a self,
        py: Python<'py>,
        f: impl FnOnce(Python<'py>) -> Result<Bound<'py, T>, E>,
    ) -> Result<Borrowed<'a, 'py, T>, E> {
        if let Some(obj) = self.get(py) {
            return Ok(obj);
        }

        let new = f(py)?.into_ptr();
        match self
            .inner
            .compare_exchange(ptr::null_mut(), new, Ordering::SeqCst, Ordering::SeqCst)
        {
            // return a `Borrowed` that borrows from the new reference
            Ok(_) => Ok(unsafe { Borrowed::from_ptr(py, new).cast_unchecked() }),

            // if it is already initialized, drop the new object and borrow from the existing reference
            Err(p) => {
                let _ = unsafe { Bound::from_owned_ptr(py, new) };
                Ok(unsafe { Borrowed::from_ptr(py, p).cast_unchecked() })
            }
        }
    }

    pub fn take(&mut self) -> Option<Py<T>> {
        NonNull::new(self.inner.swap(ptr::null_mut(), Ordering::SeqCst))
            .map(|p| unsafe { Py::from_non_null(p) })
    }

    pub fn into_inner<'py>(self, py: Python<'py>) -> Option<Bound<'py, T>> {
        unsafe {
            // need to transmute here because `self` implements `Drop`
            let inner: AtomicPtr<pyo3_ffi::PyObject> = mem::transmute(self);
            Bound::from_owned_ptr_or_opt(py, inner.into_inner()).map(|ob| ob.cast_into_unchecked())
        }
    }
}

impl<T> PyOnceAtomic<T>
where
    T: PyTypeCheck,
{
    pub fn import<'a, 'py>(
        &'a self,
        py: Python<'py>,
        module_name: &str,
        attr_name: &str,
    ) -> PyResult<Borrowed<'a, 'py, T>> {
        self.get_or_try_init(py, |py| {
            Ok(py.import(module_name)?.getattr(attr_name)?.cast_into()?)
        })
    }
}

impl<T> Drop for PyOnceAtomic<T> {
    fn drop(&mut self) {
        if let Some(p) = NonNull::new(self.inner.load(Ordering::SeqCst)) {
            let _ = unsafe { Py::<T>::from_non_null(p) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PyInt;

    #[test]
    fn test_once_atomic() {
        Python::attach(|py| {
            let mut atomic = PyOnceAtomic::new();

            assert!(atomic.get(py).is_none());

            assert_eq!(atomic.get_or_try_init(py, |_py| Err(5)).unwrap_err(), 5);
            assert!(atomic.get(py).is_none());

            assert_eq!(
                atomic
                    .get_or_try_init::<Infallible>(py, |py| Ok(PyInt::new(py, 2)))
                    .unwrap()
                    .extract::<i32>()
                    .unwrap(),
                2
            );
            assert_eq!(atomic.get(py).unwrap().extract::<i32>().unwrap(), 2);

            assert_eq!(
                atomic
                    .get_or_try_init(py, |_py| Err(5))
                    .unwrap()
                    .extract::<i32>()
                    .unwrap(),
                2
            );

            assert_eq!(atomic.take().unwrap().extract::<i32>(py).unwrap(), 2);
            assert!(atomic.into_inner(py).is_none());
        });
    }

    #[test]
    fn test_once_atomic_drop() {
        use crate::pyclass;
        use alloc::sync::Arc;
        use core::sync::atomic::AtomicBool;

        #[pyclass(frozen, crate = "crate")]
        #[derive(Debug)]
        struct RecordDrop(Arc<AtomicBool>);

        impl Drop for RecordDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        Python::attach(|py| {
            let dropped = Arc::new(AtomicBool::new(false));
            let atomic = PyOnceAtomic::new();
            atomic.get_or_init(py, |py| {
                Bound::new(py, RecordDrop(dropped.clone())).unwrap()
            });

            assert!(!dropped.load(Ordering::SeqCst));
            drop(atomic);
            assert!(dropped.load(Ordering::SeqCst));
        });
    }
}
