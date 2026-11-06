//! A lock that a panic elsewhere cannot turn into a panic here. The data
//! behind these locks is plain state a frame can read again, and a field
//! laptop is better off with a half updated console than with a dead window.

use std::sync::{Mutex, MutexGuard, PoisonError};

pub trait Relock<T> {
    /// The guard, also when a thread panicked while holding it.
    fn relock(&self) -> MutexGuard<'_, T>;
}

impl<T> Relock<T> for Mutex<T> {
    fn relock(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_poisoned_lock_is_still_taken() {
        let m = Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("holding the lock");
        })
        .join();
        assert!(m.is_poisoned());
        *m.relock() += 1;
        assert_eq!(*m.relock(), 2);
    }
}
