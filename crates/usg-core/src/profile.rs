//! Per-section profiling. Default-off; opt in by setting `USG_PROFILE=1` (concise)
//! or `USG_PROFILE=full` (verbose, includes child spans). Backed by `tracing` so
//! external collectors (`tracing-tracy`, `tracing-flame`, etc.) can also consume it.
//!
//! Usage at a call site:
//! ```ignore
//! use unity_solution_generator::profile::section;
//! let _s = section!("scan.parallel-walk", root = root_path);
//! ```
//!
//! `_s` must be held for the duration of the work — drop closes the span and
//! emits its elapsed time.

pub use tracing::Level;

/// Open a profiling section. Equivalent to `tracing::info_span!(...).entered()`,
/// but named for clarity at the call site.
#[macro_export]
macro_rules! section {
    ($name:expr) => {
        tracing::info_span!($name).entered()
    };
    ($name:expr, $($field:tt)+) => {
        tracing::info_span!($name, $($field)+).entered()
    };
}

pub use crate::section;
