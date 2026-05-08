//! Shared parallel-walk infrastructure used by both `project_scanner` and
//! `lockfile_scanner`. Each scanner previously kept its own copy of the
//! per-thread Flusher pattern recommended by `ignore`'s
//! [`ParallelVisitorBuilder`](https://docs.rs/ignore/latest/ignore/struct.WalkParallel.html#method.run)
//! docs; the two copies were drifting in subtle ways (different filter rules
//! firing in different orders).
//!
//! Pattern: each worker thread accumulates into a thread-local `Bucket`. On
//! thread teardown, the bucket is drained into a shared `Mutex<Bucket>` via
//! `Drop`. This avoids the lock-contention of locking-per-entry while keeping
//! the merge correct across non-deterministic scheduler order.

use std::sync::Mutex;

use ignore::{DirEntry, WalkBuilder, WalkState};

/// Per-thread accumulator that can be drained into a shared aggregate.
/// Implementors must support `Default` (empty start state) and a merge that
/// moves all data out of `other` into `self`.
pub trait Bucket: Default + Send {
    fn merge_from(&mut self, other: Self);
}

impl<T: Send + 'static> Bucket for Vec<T> {
    fn merge_from(&mut self, mut other: Self) {
        self.append(&mut other);
    }
}

/// Run a parallel walk with per-thread bucket accumulation. Returns the merged
/// aggregate after all threads have flushed.
///
/// `visit` is shared across worker threads (`Send + Sync`). Walk errors are
/// silently skipped — this matches the prior behaviour of both scanners
/// (filesystem races, broken symlinks, etc. shouldn't fail the whole scan).
pub fn parallel_walk<B, V>(builder: WalkBuilder, visit: V) -> B
where
    B: Bucket + 'static,
    V: Fn(&mut B, DirEntry) -> WalkState + Send + Sync,
{
    let aggregate: Mutex<B> = Mutex::new(B::default());
    let agg_ref: &Mutex<B> = &aggregate;
    let visit_ref: &V = &visit;

    builder.build_parallel().run(|| {
        struct Flusher<'a, B: Bucket> {
            local: B,
            aggregate: &'a Mutex<B>,
        }
        impl<B: Bucket> Drop for Flusher<'_, B> {
            fn drop(&mut self) {
                let local = std::mem::take(&mut self.local);
                self.aggregate.lock().unwrap().merge_from(local);
            }
        }
        let mut flusher = Flusher {
            local: B::default(),
            aggregate: agg_ref,
        };
        Box::new(move |result| {
            let Ok(entry) = result else {
                return WalkState::Continue;
            };
            visit_ref(&mut flusher.local, entry)
        })
    });

    aggregate.into_inner().unwrap()
}
