use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct ImportGuards {
    bulk_running: AtomicBool,
    entries_running: Mutex<HashSet<u64>>,
}

impl ImportGuards {
    pub fn try_start_bulk(&self) -> bool {
        self.bulk_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn finish_bulk(&self) {
        self.bulk_running.store(false, Ordering::SeqCst);
    }

    pub fn try_lock_entry(&self, id: u64) -> bool {
        if let Ok(mut guard) = self.entries_running.lock() {
            if guard.contains(&id) {
                false
            } else {
                guard.insert(id);
                true
            }
        } else {
            false
        }
    }

    pub fn release_entry(&self, id: u64) {
        if let Ok(mut guard) = self.entries_running.lock() {
            guard.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_guard_blocks_reentry() {
        let guard = ImportGuards::default();
        assert!(guard.try_start_bulk());
        assert!(!guard.try_start_bulk());
        guard.finish_bulk();
        assert!(guard.try_start_bulk());
    }

    #[test]
    fn per_entry_lock_prevents_parallel_import() {
        let guard = ImportGuards::default();
        assert!(guard.try_lock_entry(1));
        assert!(!guard.try_lock_entry(1));
        assert!(guard.try_lock_entry(2));
        guard.release_entry(1);
        assert!(guard.try_lock_entry(1));
    }
}
