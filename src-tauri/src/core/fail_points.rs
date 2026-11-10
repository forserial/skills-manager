//! Named failure points for tests of the write paths that put a skill folder
//! and its record into the library (sm-69q.20).
//!
//! Production code calls [`hit`] at the step a test needs to fail ("after the
//! copy", "the rename", "removing the backup"). Outside tests it is an empty
//! inline function that always succeeds. In tests, [`arm`] makes the named
//! point fail on the current thread until the returned guard is dropped, so
//! tests running in parallel do not see each other's failures.

#[cfg(not(test))]
#[inline(always)]
pub(crate) fn hit(_name: &'static str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
pub(crate) use test_points::{arm, hit};

#[cfg(test)]
mod test_points {
    use std::cell::RefCell;

    thread_local! {
        static ARMED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn hit(name: &'static str) -> std::io::Result<()> {
        if ARMED.with(|armed| armed.borrow().contains(&name)) {
            return Err(std::io::Error::other(format!("injected failure: {name}")));
        }
        Ok(())
    }

    /// Fail `name` on this thread until the guard is dropped.
    #[must_use]
    pub(crate) fn arm(name: &'static str) -> Armed {
        ARMED.with(|armed| armed.borrow_mut().push(name));
        Armed(name)
    }

    pub(crate) struct Armed(&'static str);

    impl Drop for Armed {
        fn drop(&mut self) {
            ARMED.with(|armed| {
                let mut armed = armed.borrow_mut();
                if let Some(at) = armed.iter().position(|n| *n == self.0) {
                    armed.remove(at);
                }
            });
        }
    }
}
