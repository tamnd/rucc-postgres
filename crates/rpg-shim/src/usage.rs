//! CPU time and peak memory of the processes the shim has waited for.
//!
//! `getrusage(RUSAGE_CHILDREN)` covers every child that has been waited for and, through them,
//! every grandchild they waited for. GCC runs `cc1`, `as` and `collect2` as children of the
//! driver, so this is the right total for a GCC call, where the resource usage of the driver
//! process alone would miss nearly all of it. The counters only grow, so the cost of one call out
//! of several is the difference of two snapshots.

/// Resource usage of reaped children, so far.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// User CPU seconds.
    pub user_seconds: f64,
    /// System CPU seconds.
    pub system_seconds: f64,
    /// The largest resident set of any one reaped child, in KiB.
    pub peak_rss_kb: u64,
}

impl Usage {
    /// The CPU spent between two snapshots, with the peak of the later one.
    ///
    /// The peak is not a counter, so it cannot be subtracted. It is the largest of every child so
    /// far, which for the first compile of a shim process is exactly that compile.
    #[must_use]
    pub fn since(self, earlier: Self) -> Self {
        Self {
            user_seconds: (self.user_seconds - earlier.user_seconds).max(0.0),
            system_seconds: (self.system_seconds - earlier.system_seconds).max(0.0),
            peak_rss_kb: self.peak_rss_kb,
        }
    }
}

/// Read the counters for reaped children, or `None` where the call is not available.
#[cfg(unix)]
#[must_use]
#[allow(unsafe_code)]
pub fn children() -> Option<Usage> {
    let mut raw = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `raw` is a valid, writable rusage and RUSAGE_CHILDREN is a valid `who`. The call
    // writes the whole struct on success, and we only read it when it reports success.
    let status = unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, raw.as_mut_ptr()) };
    if status != 0 {
        return None;
    }
    // SAFETY: getrusage returned zero, so it filled the struct.
    let raw = unsafe { raw.assume_init() };
    Some(Usage {
        user_seconds: seconds(raw.ru_utime),
        system_seconds: seconds(raw.ru_stime),
        peak_rss_kb: peak_kb(raw.ru_maxrss),
    })
}

/// No counters off Unix. Windows builds run the shim under MSYS2, where this is a later problem.
#[cfg(not(unix))]
#[must_use]
pub fn children() -> Option<Usage> {
    None
}

#[cfg(unix)]
#[allow(clippy::cast_precision_loss, clippy::useless_conversion)]
// `tv_usec` is `i64` on Linux and `i32` on macOS, so the conversion is useless on only one of them.
fn seconds(t: libc::timeval) -> f64 {
    t.tv_sec as f64 + f64::from(i32::try_from(t.tv_usec).unwrap_or(0)) / 1e6
}

/// `ru_maxrss` is KiB on Linux and bytes on macOS.
#[cfg(unix)]
fn peak_kb(maxrss: libc::c_long) -> u64 {
    let value = u64::try_from(maxrss).unwrap_or(0);
    if cfg!(target_os = "macos") {
        value / 1024
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cost_of_a_call_is_the_difference_but_the_peak_is_not() {
        let before = Usage {
            user_seconds: 1.0,
            system_seconds: 0.5,
            peak_rss_kb: 100,
        };
        let after = Usage {
            user_seconds: 1.25,
            system_seconds: 0.75,
            peak_rss_kb: 400,
        };
        let spent = after.since(before);
        assert!((spent.user_seconds - 0.25).abs() < 1e-9);
        assert!((spent.system_seconds - 0.25).abs() < 1e-9);
        assert_eq!(spent.peak_rss_kb, 400);
    }

    #[cfg(unix)]
    #[test]
    fn a_reaped_child_shows_up_in_the_counters() {
        std::process::Command::new("true").status().unwrap();
        assert!(children().is_some());
    }
}
