//! The wall clock as the transports and the frame layer read it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch (`time.time()`).
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
