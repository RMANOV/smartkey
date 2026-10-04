//! Saturating unload-counter updates compatible with stable Rust 1.94.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Preserve the original SeqCst read-modify-write, including zero-to-zero.
pub(crate) fn saturating_decrement(counter: &AtomicUsize) {
    let mut observed = counter.load(Ordering::SeqCst);
    loop {
        match counter.compare_exchange_weak(
            observed,
            observed.saturating_sub(1),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return,
            Err(current) => observed = current,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn zero_remains_zero() {
        let counter = AtomicUsize::new(0);
        for _ in 0..10 {
            saturating_decrement(&counter);
        }
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn one_reaches_zero() {
        let counter = AtomicUsize::new(1);
        saturating_decrement(&counter);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn positive_decrements_once() {
        let counter = AtomicUsize::new(42);
        saturating_decrement(&counter);
        assert_eq!(counter.load(Ordering::SeqCst), 41);
    }

    #[test]
    fn maximum_decrements_once() {
        let counter = AtomicUsize::new(usize::MAX);
        saturating_decrement(&counter);
        assert_eq!(counter.load(Ordering::SeqCst), usize::MAX - 1);
    }

    fn concurrent_decrements(initial: usize, per_thread: usize) -> usize {
        let counter = Arc::new(AtomicUsize::new(initial));
        let start = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let counter = Arc::clone(&counter);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    for _ in 0..per_thread {
                        saturating_decrement(&counter);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        counter.load(Ordering::SeqCst)
    }

    #[test]
    fn concurrent_exact_decrements_reach_zero() {
        assert_eq!(concurrent_decrements(800, 100), 0);
    }

    #[test]
    fn concurrent_excess_decrements_stay_zero() {
        assert_eq!(concurrent_decrements(17, 100), 0);
    }

    #[test]
    fn concurrent_balanced_updates_preserve_cushion() {
        let counter = Arc::new(AtomicUsize::new(1000));
        let start = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let counter = Arc::clone(&counter);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    for _ in 0..100 {
                        counter.fetch_add(1, Ordering::SeqCst);
                        saturating_decrement(&counter);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1000);
    }
}
