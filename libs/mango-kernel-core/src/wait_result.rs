//! WaitQueue waiting result enum.
//!
//! Extracted from `os/src/task/manager.rs`.
//! Pure logic — no kernel dependencies, no I/O, no global state.
//!
//! The `unwrap_or_else` method maps internal errno constants through a
//! caller-provided closure, matching the kernel's `SyscallErr::ERESTART` and
//! `SyscallErr::EAGAIN` semantics.

/// Internal errno constants matching the kernel's `SyscallErr` discriminants.
const ERESTART: isize = -85;
const EAGAIN: isize = -11;

/// Result of waiting on a `WaitQueue`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitResult {
    /// Condition satisfied, carrying a caller-defined return value.
    Ready(isize),
    /// Interrupted by an actionable signal.
    Interrupted,
    /// Deadline reached.
    TimedOut,
}

impl WaitResult {
    /// Convert the waiting result into a syscall return value.
    ///
    /// `Ready` returns its inner value directly; other variants encode their
    /// semantics through the caller-provided conversion function.
    pub fn unwrap_or_else(self, f: impl FnOnce(isize) -> isize) -> isize {
        match self {
            WaitResult::Ready(value) => value,
            WaitResult::Interrupted => f(-(ERESTART)),
            WaitResult::TimedOut => f(-(EAGAIN)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_returns_value() {
        assert_eq!(WaitResult::Ready(42).unwrap_or_else(|_| panic!()), 42);
    }

    #[test]
    fn ready_zero() {
        assert_eq!(WaitResult::Ready(0).unwrap_or_else(|_| panic!()), 0);
    }

    #[test]
    fn ready_negative_value() {
        assert_eq!(
            WaitResult::Ready(-1).unwrap_or_else(|_| panic!()),
            -1
        );
    }

    #[test]
    fn interrupted_calls_closure() {
        let result = WaitResult::Interrupted.unwrap_or_else(|e| e);
        // ERESTART = -85, so f(-(-85)) = f(85) = 85
        assert_eq!(result, 85);
    }

    #[test]
    fn timed_out_calls_closure() {
        let result = WaitResult::TimedOut.unwrap_or_else(|e| e);
        // EAGAIN = -11, so f(-(-11)) = f(11) = 11
        assert_eq!(result, 11);
    }

    #[test]
    fn interrupted_closure_receives_correct_errno() {
        let mut captured = 0isize;
        let _ = WaitResult::Interrupted.unwrap_or_else(|e| {
            captured = e;
            0
        });
        assert_eq!(captured, 85);
    }

    #[test]
    fn timed_out_closure_receives_correct_errno() {
        let mut captured = 0isize;
        let _ = WaitResult::TimedOut.unwrap_or_else(|e| {
            captured = e;
            0
        });
        assert_eq!(captured, 11);
    }
}
