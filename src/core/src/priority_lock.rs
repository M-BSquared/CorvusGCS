//! The bridge's command lock: re-entrant, and abort commands go first
//! (`priority_lock.py`).
//!
//! Every operator command is serialised through one lock so a command's send
//! and its ACK cannot interleave with another's. A plain lock hands off in
//! arrival order, so an RTL pressed during a parameter upload would wait
//! behind every remaining write. Here, while any abort waits or runs, routine
//! acquirers stand aside, so an abort waits for at most the one operation
//! already in progress.
//!
//! Re-entrant, because the command paths nest (takeoff calls arm): a thread
//! that holds the lock passes the gate, since the abort waiting on it is
//! waiting for that very thread.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::ThreadId;

const ABORT_LOG: usize = 64;

#[derive(Default)]
struct State {
    owner: Option<ThreadId>,
    depth: usize,
    aborts_waiting: usize,
    abort_count: u64,
    abort_log: VecDeque<(u64, ThreadId)>,
}

#[derive(Default)]
pub struct PriorityLock {
    state: Mutex<State>,
    changed: Condvar,
}

/// Held while a command runs; releases on drop.
pub struct Guard<'a> {
    lock: &'a PriorityLock,
    registered: bool,
}

impl PriorityLock {
    pub fn new() -> PriorityLock {
        PriorityLock::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn acquire(&self, priority: bool) -> Guard<'_> {
        let me = std::thread::current().id();
        let mut s = self.state();
        if s.owner == Some(me) {
            s.depth += 1;
            return Guard {
                lock: self,
                registered: false,
            };
        }
        if priority {
            s.aborts_waiting += 1;
            s.abort_count += 1;
            let n = s.abort_count;
            s.abort_log.push_back((n, me));
            if s.abort_log.len() > ABORT_LOG {
                s.abort_log.pop_front();
            }
            while s.owner.is_some() {
                s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        } else {
            while s.owner.is_some() || s.aborts_waiting > 0 {
                s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        }
        s.owner = Some(me);
        s.depth = 1;
        Guard {
            lock: self,
            registered: priority,
        }
    }

    /// The lock for routine work: stands aside while an abort is waiting or running.
    pub fn lock(&self) -> Guard<'_> {
        self.acquire(false)
    }

    /// The lock for a command that must not queue behind routine work.
    pub fn priority(&self) -> Guard<'_> {
        self.acquire(true)
    }

    /// Is an abort queued or running right now? A long routine operation
    /// (a mission download over a radio) asks between steps and gives way.
    pub fn abort_waiting(&self) -> bool {
        self.state().aborts_waiting > 0
    }

    /// A point to ask `aborted_since` about later.
    pub fn abort_mark(&self) -> u64 {
        self.state().abort_count
    }

    /// Has another thread's abort registered since `mark`? For a sequence
    /// that does not hold the lock between its steps (takeoff: mode, arm,
    /// takeoff): an operator who pressed Disarm, Land or RTL meanwhile has
    /// taken the aircraft back. The calling thread's own aborts do not count.
    pub fn aborted_since(&self, mark: u64) -> bool {
        let me = std::thread::current().id();
        let s = self.state();
        if s.abort_count == mark {
            return false;
        }
        if s.abort_count - mark > s.abort_log.len() as u64 {
            return true;
        }
        s.abort_log.iter().any(|(n, id)| *n > mark && *id != me)
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let mut s = self.lock.state();
        s.depth -= 1;
        if s.depth == 0 {
            s.owner = None;
        }
        if self.registered {
            s.aborts_waiting -= 1;
        }
        self.lock.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn it_is_re_entrant_for_both_kinds() {
        let l = PriorityLock::new();
        let _a = l.lock();
        let _b = l.lock();
        let _c = l.priority();
        assert!(!l.abort_waiting());
    }

    #[test]
    fn an_abort_goes_before_routine_work_that_queued_first() {
        let lock = Arc::new(PriorityLock::new());
        let (tx, rx) = mpsc::channel();
        let held = lock.lock();
        let mut threads = Vec::new();
        for name in ["routine-1", "routine-2"] {
            let (l, tx) = (lock.clone(), tx.clone());
            threads.push(std::thread::spawn(move || {
                let _g = l.lock();
                tx.send(name).unwrap();
            }));
        }
        std::thread::sleep(Duration::from_millis(100));
        let (l, t) = (lock.clone(), tx.clone());
        threads.push(std::thread::spawn(move || {
            let _g = l.priority();
            t.send("abort").unwrap();
        }));
        std::thread::sleep(Duration::from_millis(100));
        assert!(lock.abort_waiting());
        drop(held);
        let order: Vec<&str> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(order[0], "abort");
        for t in threads {
            t.join().unwrap();
        }
        assert!(!lock.abort_waiting());
    }

    #[test]
    fn routine_work_stands_aside_while_an_abort_runs() {
        let lock = Arc::new(PriorityLock::new());
        let abort = lock.priority();
        let (l, (tx, rx)) = (lock.clone(), mpsc::channel());
        let t = std::thread::spawn(move || {
            let _g = l.lock();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        drop(abort);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        t.join().unwrap();
    }

    #[test]
    fn aborts_by_others_count_and_ones_own_do_not() {
        let lock = Arc::new(PriorityLock::new());
        let mark = lock.abort_mark();
        assert!(!lock.aborted_since(mark));
        drop(lock.priority());
        assert!(!lock.aborted_since(mark), "own abort");
        let l = lock.clone();
        std::thread::spawn(move || drop(l.priority())).join().unwrap();
        assert!(lock.aborted_since(mark));
        assert!(!lock.aborted_since(lock.abort_mark()));
    }

    #[test]
    fn more_aborts_than_the_log_holds_still_count() {
        let lock = Arc::new(PriorityLock::new());
        let mark = lock.abort_mark();
        let l = lock.clone();
        std::thread::spawn(move || {
            for _ in 0..100 {
                drop(l.priority());
            }
        })
        .join()
        .unwrap();
        assert!(lock.aborted_since(mark));
        let mark2 = lock.abort_mark();
        for _ in 0..100 {
            drop(lock.priority());
        }
        assert!(lock.aborted_since(mark2), "too many to tell apart, so assume the worst");
    }
}
