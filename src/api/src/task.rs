//! Work that waits for answers, written as `async` and moved along by its owner:
//! a plugin has no thread to block, so its owner polls it on every tick, and a
//! tick follows every answer that arrives.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Waker};

pub struct Task(Option<Pin<Box<dyn Future<Output = ()>>>>);

impl Task {
    pub fn new(work: impl Future<Output = ()> + 'static) -> Task {
        Task(Some(Box::pin(work)))
    }

    /// Goes as far as it can now. True once it is done.
    pub fn poll(&mut self) -> bool {
        if let Some(work) = self.0.as_mut() {
            if work.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_ready() {
                self.0 = None;
            }
        }
        self.0.is_none()
    }

    pub fn done(&self) -> bool {
        self.0.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::bg::Pending;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn a_task_goes_on_when_what_it_waits_for_is_there() {
        let (first, fill_first) = Pending::<u8>::slot();
        let (second, fill_second) = Pending::<u8>::slot();
        let sum = Rc::new(Cell::new(0));
        let out = sum.clone();
        let mut task = Task::new(async move {
            let a = first.await;
            out.set(a);
            out.set(a + second.await);
        });
        assert!(!task.poll() && sum.get() == 0);
        fill_first.fill(2);
        assert!(!task.poll() && sum.get() == 2);
        fill_second.fill(3);
        assert!(task.poll() && sum.get() == 5);
        assert!(task.poll() && task.done());
    }
}
