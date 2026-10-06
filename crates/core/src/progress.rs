//! Progress reporting and cancellation shared by the CLI and the GUI. The core never prints:
//! it emits [`Event`]s to whatever hook the front end installed, and polls [`check`] in long loops.
use crate::{Error, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub enum Event {
    /// A step that has no meaningful percentage ("Розпаковую…").
    Status(String),
    /// Bytes received so far of `total` (when the server says).
    Download { name: String, done: u64, total: Option<u64> },
}

type Hook = Arc<dyn Fn(Event) + Send + Sync>;

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn set_hook(f: impl Fn(Event) + Send + Sync + 'static) {
    *HOOK.lock().unwrap() = Some(Arc::new(f));
}

/// Drops the hook (and with it anything it captured, e.g. a channel sender).
pub fn clear_hook() {
    *HOOK.lock().unwrap() = None;
}

pub fn emit(e: Event) {
    let h = HOOK.lock().unwrap().clone();
    if let Some(h) = h {
        h(e);
    }
}

pub fn status(s: impl Into<String>) {
    emit(Event::Status(s.into()));
}

/// Ask the running operation to stop at the next checkpoint.
pub fn cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Forget an earlier cancel; call before starting an operation.
pub fn reset() {
    CANCEL.store(false, Ordering::SeqCst);
}

pub fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

/// `Err(Cancelled)` once [`cancel`] was called. Put it in loops that can take long.
pub fn check() -> Result<()> {
    if cancelled() { Err(Error::Cancelled) } else { Ok(()) }
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_reach_the_hook_and_cancel_is_sticky_until_reset() {
        let _g = super::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        set_hook(move |e| s2.lock().unwrap().push(format!("{e:?}")));
        status("x");
        clear_hook();
        status("ignored");
        assert_eq!(seen.lock().unwrap().len(), 1);

        reset();
        assert!(check().is_ok());
        cancel();
        assert!(matches!(check(), Err(Error::Cancelled)));
        reset();
        assert!(check().is_ok());
    }
}
