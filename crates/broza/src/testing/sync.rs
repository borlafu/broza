//! The one lock helper every fake uses.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock a mutex, recovering the value when another test thread poisoned it.
///
/// A fake must not turn one failing test into a cascade of poisoned-lock panics in
/// every test that shares it, so a poisoned guard is taken as-is.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::lock;

    #[test]
    fn a_poisoned_lock_still_yields_its_value() {
        let mutex = Mutex::new(7);
        let _ = std::panic::catch_unwind(|| {
            let _guard = lock(&mutex);
            panic!("poison it");
        });

        assert!(mutex.is_poisoned());
        assert_eq!(*lock(&mutex), 7);
    }
}
