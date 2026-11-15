//! Run blocking work through [`Platform::spawn_blocking`] and await its
//! result from any async runtime, without depending on one.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use crate::platform::Platform;

struct Shared<T> {
    value: Option<T>,
    /// The task finished (with a value) or was dropped without running.
    done: bool,
    waker: Option<Waker>,
}

/// Completes the task when dropped, so a task that panicked or was never run
/// wakes the waiting future instead of leaving it pending forever.
struct Sender<T>(Arc<Mutex<Shared<T>>>);

impl<T> Sender<T> {
    fn send(self, value: T) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).value = Some(value);
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = {
            let mut shared = self.0.lock().unwrap_or_else(|e| e.into_inner());
            shared.done = true;
            shared.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// The result of [`run_blocking`]: `Err` with the panic message if the task
/// panicked, `None` if the platform dropped it without running it.
pub(crate) struct BlockingTask<T>(Arc<Mutex<Shared<T>>>);

impl<T> Future for BlockingTask<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut shared = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if shared.done {
            return Poll::Ready(shared.value.take());
        }
        match &shared.waker {
            Some(w) if w.will_wake(cx.waker()) => {}
            _ => shared.waker = Some(cx.waker().clone()),
        }
        Poll::Pending
    }
}

pub(crate) fn run_blocking<P, T, F>(
    platform: &P,
    f: F,
) -> BlockingTask<std::result::Result<T, String>>
where
    P: Platform + ?Sized,
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let shared = Arc::new(Mutex::new(Shared {
        value: None,
        done: false,
        waker: None,
    }));
    let sender = Sender(shared.clone());
    platform.spawn_blocking(Box::new(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .map_err(|payload| panic_message(payload.as_ref()));
        sender.send(result)
    }));
    BlockingTask(shared)
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".into()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{path::PathBuf, sync::mpsc, task::Wake, thread::Thread};

    use serde_json::Value;

    use crate::{BaseDir, EventHandler, Result};

    /// Minimal executor for tests.
    pub(crate) fn block_on<F: Future>(f: F) -> F::Output {
        struct ThreadWaker(Thread);
        impl Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut f = std::pin::pin!(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park();
        }
    }

    /// Runs tasks on threads, or drops them unrun.
    struct Threads {
        run: bool,
    }

    impl Platform for Threads {
        type ListenerId = ();
        fn emit(&self, _: &str, _: Value) -> std::result::Result<(), String> {
            Ok(())
        }
        fn listen(&self, _: &str, _: EventHandler) {}
        fn unlisten(&self, _: ()) {}
        fn base_dir(&self, _: BaseDir) -> Result<PathBuf> {
            Ok(PathBuf::new())
        }
        fn spawn_blocking(&self, task: Box<dyn FnOnce() + Send + 'static>) {
            if self.run {
                std::thread::spawn(task);
            }
        }
    }

    #[test]
    fn completes() {
        let (tx, rx) = mpsc::channel::<()>();
        let task = run_blocking(&Threads { run: true }, move || {
            // Make sure the future is polled (and parks) before completion.
            rx.recv().unwrap();
            42
        });
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            tx.send(()).unwrap();
        });
        assert_eq!(block_on(task), Some(Ok(42)));
    }

    #[test]
    fn panics_and_dropped_tasks_resolve() {
        let task = run_blocking(&Threads { run: true }, || -> u32 { panic!("boom") });
        assert_eq!(block_on(task), Some(Err("boom".to_string())));
        let task = run_blocking(&Threads { run: false }, || 1);
        assert_eq!(block_on(task), None);
    }
}
