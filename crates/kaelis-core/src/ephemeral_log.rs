//! Bounded RAM-only instance log ring buffer. It is discarded with the
//! process unless the user explicitly exports it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

const MAX_LINE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct EphemeralLog {
    inner: Arc<Mutex<VecDeque<String>>>,
    capacity: usize,
}

impl EphemeralLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<String>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn push(&self, line: impl AsRef<str>) {
        if self.capacity == 0 {
            return;
        }
        let line = line.as_ref();
        let bounded = if line.len() <= MAX_LINE_BYTES {
            line.to_owned()
        } else {
            let mut end = MAX_LINE_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line[..end].to_owned()
        };
        let mut guard = self.lock();
        if guard.len() == self.capacity {
            guard.pop_front();
        }
        guard.push_back(bounded);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lock().iter().cloned().collect()
    }

    /// Explicit user-triggered export only. Nothing writes logs automatically.
    pub fn export(&self, path: &std::path::Path) -> std::io::Result<()> {
        let guard = self.lock();
        let bytes = guard.iter().fold(String::new(), |mut output, line| {
            output.push_str(line);
            output.push('\n');
            output
        });
        std::fs::write(path, bytes)
    }

    pub fn clear(&self) {
        self.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_only_the_newest_lines_within_capacity() {
        let log = EphemeralLog::new(2);
        log.push("first");
        log.push("second");
        log.push("third");

        assert_eq!(log.snapshot(), ["second", "third"]);
    }

    #[test]
    fn truncates_long_lines_without_splitting_utf8() {
        let log = EphemeralLog::new(1);
        log.push("€".repeat(MAX_LINE_BYTES / 3 + 1));

        let snapshot = log.snapshot();
        assert_eq!(snapshot[0].len(), MAX_LINE_BYTES - 1);
        assert!(snapshot[0].is_char_boundary(snapshot[0].len()));
    }

    #[test]
    fn zero_capacity_discards_lines() {
        let log = EphemeralLog::new(0);
        log.push("not retained");
        assert!(log.snapshot().is_empty());
    }
}
