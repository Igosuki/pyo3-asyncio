#![warn(missing_docs)]

//! Rust Bindings to the Python Asyncio Event Loop
//!
//! # Motivation
//!
//! This crate aims to provide a convenient interface to manage the interop between Python and
//! Rust's async/await models. It supports conversions between Rust and Python futures and manages
//! the event loops for both languages. Python's threading model and GIL can make this interop a bit
//! trickier than one might expect, so there are a few caveats that users should be aware of.
//!
//! ## Why Two Event Loops
//!
//! Currently, we don't have a way to run Rust futures directly on Python's event loop. Likewise,
//! Python's coroutines cannot be directly spawned on a Rust event loop. The two coroutine models
//! require some additional assistance from their event loops, so in all likelihood they will need
//! a new _unique_ event loop that addresses the needs of both languages if the coroutines are to
//! be run on the same loop.
//!
//! It's not immediately clear that this would provide worthwhile performance wins either, so in the
//! interest of keeping things simple, this crate creates and manages the Python event loop and
//! handles the communication between separate Rust event loops.
//!
//! ## Python's Event Loop
//!
//! Python is very picky about the threads used by the `asyncio` executor. In particular, it needs
//! to have control over the main thread in order to handle signals like CTRL-C correctly. This
//! means that Cargo's default test harness will no longer work since it doesn't provide a method of
//! overriding the main function to add our event loop initialization and finalization.
//!
//! ## Rust's Event Loop
//!
//! Currently only the async-std and Tokio runtimes are supported by this crate.
//!
//! > _In the future, more runtimes may be supported for Rust._
//!
//! ## Features
//!
//! Items marked with
//! <span
//!   class="module-item stab portability"
//!   style="display: inline; border-radius: 3px; padding: 2px; font-size: 80%; line-height: 1.2;"
//! ><code>attributes</code></span>
//! are only available when the `attributes` Cargo feature is enabled:
//!
//! ```toml
//! [dependencies.pyo3-asyncio]
//! version = "0.13.0"
//! features = ["attributes"]
//! ```
//!
//! Items marked with
//! <span
//!   class="module-item stab portability"
//!   style="display: inline; border-radius: 3px; padding: 2px; font-size: 80%; line-height: 1.2;"
//! ><code>async-std-runtime</code></span>
//! are only available when the `async-std-runtime` Cargo feature is enabled:
//!
//! ```toml
//! [dependencies.pyo3-asyncio]
//! version = "0.13.0"
//! features = ["async-std-runtime"]
//! ```
//!
//! Items marked with
//! <span
//!   class="module-item stab portability"
//!   style="display: inline; border-radius: 3px; padding: 2px; font-size: 80%; line-height: 1.2;"
//! ><code>tokio-runtime</code></span>
//! are only available when the `tokio-runtime` Cargo feature is enabled:
//!
//! ```toml
//! [dependencies.pyo3-asyncio]
//! version = "0.13.0"
//! features = ["tokio-runtime"]
//! ```
//!
//! Items marked with
//! <span
//!   class="module-item stab portability"
//!   style="display: inline; border-radius: 3px; padding: 2px; font-size: 80%; line-height: 1.2;"
//! ><code>testing</code></span>
//! are only available when the `testing` Cargo feature is enabled:
//!
//! ```toml
//! [dependencies.pyo3-asyncio]
//! version = "0.13.0"
//! features = ["testing"]
//! ```

/// <span class="module-item stab portability" style="display: inline; border-radius: 3px; padding: 2px; font-size: 80%; line-height: 1.2;"><code>testing</code></span> Utilities for writing PyO3 Asyncio tests
#[cfg(feature = "testing")]
#[doc(inline)]
pub mod testing;

/// <span class="module-item stab portability" style="display: inline; border-radius: 3px; padding: 2px; font-size: 80%; line-height: 1.2;"><code>async-std-runtime</code></span> PyO3 Asyncio functions specific to the async-std runtime
#[cfg(feature = "async-std")]
#[doc(inline)]
pub mod async_std;

/// <span class="module-item stab portability" style="display: inline; border-radius: 3px; padding: 2px; font-size: 80%; line-height: 1.2;"><code>tokio-runtime</code></span> PyO3 Asyncio functions specific to the tokio runtime
#[cfg(feature = "tokio-runtime")]
#[doc(inline)]
pub mod tokio;

/// Generic implementations of PyO3 Asyncio utilities that can be used for any Rust runtime
pub mod generic;

use std::{
    convert::TryFrom,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::channel::oneshot;
use once_cell::sync::OnceCell;
use pyo3::{exceptions::PyKeyboardInterrupt, prelude::*, PyNativeType};

/// Re-exported for #[test] attributes
#[cfg(all(feature = "attributes", feature = "testing"))]
pub use inventory;

/// Test README
#[doc(hidden)]
pub mod doc_test {
    #[allow(unused)]
    macro_rules! doc_comment {
        ($x:expr, $module:item) => {
            #[doc = $x]
            $module
        };
    }

    #[allow(unused)]
    macro_rules! doctest {
        ($x:expr, $y:ident) => {
            doc_comment!(include_str!($x), mod $y {});
        };
    }

    #[cfg(all(
        feature = "async-std-runtime",
        feature = "tokio-runtime",
        feature = "attributes"
    ))]
    doctest!("../README.md", readme_md);
}

const EXPECT_INIT: &str = "PyO3 Asyncio has not been initialized";

static ASYNCIO: OnceCell<PyObject> = OnceCell::new();
static ENSURE_FUTURE: OnceCell<PyObject> = OnceCell::new();
static EVENT_LOOP: OnceCell<PyObject> = OnceCell::new();
static EXECUTOR: OnceCell<PyObject> = OnceCell::new();
static CALL_SOON: OnceCell<PyObject> = OnceCell::new();
static CREATE_FUTURE: OnceCell<PyObject> = OnceCell::new();

fn ensure_future(py: Python) -> &PyAny {
    ENSURE_FUTURE.get().expect(EXPECT_INIT).as_ref(py)
}

#[allow(clippy::needless_doctest_main)]
/// Wraps the provided function with the initialization and finalization for PyO3 Asyncio
///
/// This function **_MUST_** be called from the main thread.
///
/// # Arguments
/// * `py` - The current PyO3 GIL guard
/// * `f` - The function to call in between intialization and finalization
///
/// # Examples
///
/// ```
/// use pyo3::prelude::*;
///
/// fn main() {
///     Python::with_gil(|py| {
///         pyo3_asyncio::with_runtime(py, || {
///             println!("PyO3 Asyncio Initialized!");
///             Ok(())
///         })
///         .map_err(|e| {
///             e.print_and_set_sys_last_vars(py);  
///         })
///         .unwrap();
///     })
/// }
/// ```
pub fn with_runtime<F, R>(py: Python, f: F) -> PyResult<R>
where
    F: FnOnce() -> PyResult<R>,
{
    try_init(py)?;

    let result = (f)()?;

    try_close(py)?;

    Ok(result)
}

/// Attempt to initialize the Python and Rust event loops
///
/// - Must be called before any other pyo3-asyncio functions.
/// - Calling `try_init` a second time returns `Ok(())` and does nothing.
///   > In future versions this may return an `Err`.
pub fn try_init(py: Python) -> PyResult<()> {
    EVENT_LOOP.get_or_try_init(|| -> PyResult<PyObject> {
        let asyncio = py.import("asyncio")?;
        let ensure_future = asyncio.getattr("ensure_future")?;
        let event_loop = asyncio.call_method0("get_event_loop")?;
        let executor = py
            .import("concurrent.futures.thread")?
            .getattr("ThreadPoolExecutor")?
            .call0()?;
        event_loop.call_method1("set_default_executor", (executor,))?;
        let call_soon = event_loop.getattr("call_soon_threadsafe")?;
        let create_future = event_loop.getattr("create_future")?;

        ASYNCIO.get_or_init(|| asyncio.into());
        ENSURE_FUTURE.get_or_init(|| ensure_future.into());
        EXECUTOR.get_or_init(|| executor.into());
        CALL_SOON.get_or_init(|| call_soon.into());
        CREATE_FUTURE.get_or_init(|| create_future.into());
        Ok(event_loop.into())
    })?;

    Ok(())
}

/// Get a reference to the Python Event Loop from Rust
pub fn get_event_loop(py: Python) -> &PyAny {
    EVENT_LOOP.get().expect(EXPECT_INIT).as_ref(py)
}

/// Run the event loop forever
///
/// This can be called instead of `run_until_complete` to run the event loop
/// until `stop` is called rather than driving a future to completion.
///
/// After this function returns, the event loop can be resumed with either `run_until_complete` or
/// [`crate::run_forever`]
///
/// # Arguments
/// * `py` - The current PyO3 GIL guard
///
/// # Examples
///
/// ```
/// # use std::time::Duration;
/// # use pyo3::prelude::*;
/// # Python::with_gil(|py| {
/// # pyo3_asyncio::with_runtime(py, || {
/// // Wait 1 second, then stop the event loop
/// # #[cfg(feature = "async-std-runtime")]
/// async_std::task::spawn(async move {
///     async_std::task::sleep(Duration::from_secs(1)).await;
///     Python::with_gil(|py| {
///         let event_loop = pyo3_asyncio::get_event_loop(py);
///         
///         event_loop
///             .call_method1(
///                 "call_soon_threadsafe",
///                 (event_loop
///                     .getattr("stop")
///                     .map_err(|e| e.print_and_set_sys_last_vars(py))
///                     .unwrap(),),
///                 )
///                 .map_err(|e| e.print_and_set_sys_last_vars(py))
///                 .unwrap();
///     })
/// });        
///
/// // block until stop is called
/// # #[cfg(feature = "async-std-runtime")]
/// pyo3_asyncio::run_forever(py)?;
/// # Ok(())
/// # })
/// # .map_err(|e| e.print_and_set_sys_last_vars(py))
/// # .unwrap();
/// # })
pub fn run_forever(py: Python) -> PyResult<()> {
    if let Err(e) = get_event_loop(py).call_method0("run_forever") {
        if e.is_instance::<PyKeyboardInterrupt>(py) {
            Ok(())
        } else {
            Err(e)
        }
    } else {
        Ok(())
    }
}

/// Shutdown the event loops and perform any necessary cleanup
pub fn try_close(py: Python) -> PyResult<()> {
    // Shutdown the executor and wait until all threads are cleaned up
    EXECUTOR
        .get()
        .expect(EXPECT_INIT)
        .call_method0(py, "shutdown")?;

    get_event_loop(py).call_method0("stop")?;
    get_event_loop(py).call_method0("close")?;
    Ok(())
}

#[pyclass]
struct PyTaskCompleter {
    tx: Option<oneshot::Sender<PyResult<PyObject>>>,
}

#[pymethods]
impl PyTaskCompleter {
    #[call]
    #[args(task)]
    pub fn __call__(&mut self, task: &PyAny) -> PyResult<()> {
        debug_assert!(task.call_method0("done")?.extract()?);

        let result = match task.call_method0("result") {
            Ok(val) => Ok(val.into()),
            Err(e) => Err(e),
        };

        // unclear to me whether or not this should be a panic or silent error.
        //
        // calling PyTaskCompleter twice should not be possible, but I don't think it really hurts
        // anything if it happens.
        if let Some(tx) = self.tx.take() {
            if tx.send(result).is_err() {
                // cancellation is not an error
            }
        }

        Ok(())
    }
}

#[pyclass]
struct PyEnsureFuture {
    awaitable: PyObject,
    tx: Option<oneshot::Sender<PyResult<PyObject>>>,
}

#[pymethods]
impl PyEnsureFuture {
    #[call]
    pub fn __call__(&mut self) -> PyResult<()> {
        Python::with_gil(|py| {
            let task = ensure_future(py).call1((self.awaitable.as_ref(py),))?;
            let on_complete = PyTaskCompleter { tx: self.tx.take() };
            task.call_method1("add_done_callback", (on_complete,))?;

            Ok(())
        })
    }
}

pub struct PyFuture {
    rx: oneshot::Receiver<PyResult<PyObject>>,
}

impl Future for PyFuture {
    type Output = PyResult<PyObject>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        match Pin::new(&mut self.as_mut().rx).poll(cx) {
            Poll::Ready(Ok(item)) => Poll::Ready(item),
            Poll::Ready(Err(_)) => Python::with_gil(|py| {
                Poll::Ready(Err(PyErr::from_instance(
                    ASYNCIO
                        .get()
                        .expect(EXPECT_INIT)
                        .call_method0(py, "CancelledError")?
                        .as_ref(py),
                )))
            }),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<'p> TryFrom<&'p PyAny> for PyFuture {
    type Error = PyErr;

    fn try_from(awaitable: &'p PyAny) -> PyResult<Self> {
        let py = awaitable.py();
        let (tx, rx) = oneshot::channel();
        CALL_SOON.get().expect(EXPECT_INIT).call1(
            py,
            (PyEnsureFuture {
                awaitable: awaitable.into(),
                tx: Some(tx),
            },),
        )?;
        Ok(Self { rx })
    }
}

/// Convert a Python `awaitable` into a Rust Future
///
/// This function converts the `awaitable` into a Python Task using `run_coroutine_threadsafe`. A
/// completion handler sends the result of this Task through a
/// `futures::channel::oneshot::Sender<PyResult<PyObject>>` and the future returned by this function
/// simply awaits the result through the `futures::channel::oneshot::Receiver<PyResult<PyObject>>`.
///
/// # Arguments
/// * `awaitable` - The Python `awaitable` to be converted
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use pyo3::prelude::*;
///
/// const PYTHON_CODE: &'static str = r#"
/// import asyncio
///
/// async def py_sleep(duration):
///     await asyncio.sleep(duration)
/// "#;
///
/// async fn py_sleep(seconds: f32) -> PyResult<()> {
///     let test_mod = Python::with_gil(|py| -> PyResult<PyObject> {
///         Ok(
///             PyModule::from_code(
///                 py,
///                 PYTHON_CODE,
///                 "test_into_future/test_mod.py",
///                 "test_mod"
///             )?
///             .into()
///         )
///     })?;
///
///     Python::with_gil(|py| {
///         pyo3_asyncio::into_future(
///             test_mod
///                 .call_method1(py, "py_sleep", (seconds.into_py(py),))?
///                 .as_ref(py),
///         )
///     })?
///     .await?;
///     Ok(())    
/// }
/// ```
pub fn into_future(awaitable: &PyAny) -> PyResult<impl Future<Output = PyResult<PyObject>> + Send> {
    PyFuture::try_from(awaitable)
}

fn dump_err(py: Python<'_>) -> impl FnOnce(PyErr) + '_ {
    move |e| {
        // We can't display Python exceptions via std::fmt::Display,
        // so print the error here manually.
        e.print_and_set_sys_last_vars(py);
    }
}
