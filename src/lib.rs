//! `tokio_retry` but simple.
//!
//! Pass your delays as a `&[u64]` (milliseconds) and an async closure. Done.
//!
//! ```rust
//! use tokio_retry_simple::{retry, RetryError};
//!
//! # async fn example() -> Result<(), String> {
//! let val = retry(&[1000, 2000, 4000], || async {
//!     do_request().await.map_err(RetryError::Transient)
//! })
//! .call()
//! .await?;
//! # Ok(())
//! # }
//! # async fn do_request() -> Result<String, String> { Ok("ok".into()) }
//! ```
//!
//! No strategy objects, no iterator-based delay generators, no traits.
//!
//! Optionally, you can attach hooks for logging or metrics:
//!
//! ```rust
//! # use tokio_retry_simple::{retry, RetryError};
//! # async fn example() -> Result<(), String> {
//! let val = retry(&[1000, 2000], || async {
//!     do_request().await.map_err(RetryError::Transient)
//! })
//! .before_all(|| println!("starting"))
//! .before_attempt(|info| println!("attempt {}", info.attempt))
//! .after_attempt(|info| {
//!     if let Some(d) = info.next_delay {
//!         println!("  retrying in {d:?}");
//!     }
//! })
//! .after_all(|| println!("done"))
//! .call()
//! .await?;
//! # Ok(())
//! # }
//! # async fn do_request() -> Result<String, String> { Ok("ok".into()) }
//! ```

use std::fmt;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use tokio::time::Instant;

/// Classifies an error as retryable or fatal.
///
/// Your operation closure returns `Result<T, RetryError<E>>`. The retry loop
/// inspects the variant to decide whether to keep going or bail.
///
/// Works naturally with `?`:
/// ```rust
/// # use tokio_retry_simple::RetryError;
/// # async fn connect() -> Result<Connection, String> { Ok(Connection) }
/// # struct Connection;
/// # impl Connection { async fn query(&self) -> Result<(), String> { Ok(()) } }
/// # async fn example() -> Result<(), RetryError<String>> {
/// let conn = connect().await.map_err(RetryError::Transient)?;
/// let data = conn.query().await.map_err(RetryError::Permanent)?;
/// # Ok(())
/// # }
/// ```
pub enum RetryError<E> {
    /// Retryable — will try again if attempts remain.
    Transient(E),
    /// Fatal — stops immediately, no more attempts.
    Permanent(E),
}

impl<E: fmt::Debug> fmt::Debug for RetryError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetryError::Transient(e) => f.debug_tuple("Transient").field(e).finish(),
            RetryError::Permanent(e) => f.debug_tuple("Permanent").field(e).finish(),
        }
    }
}

impl<E: fmt::Display> fmt::Display for RetryError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetryError::Transient(e) => write!(f, "transient error: {e}"),
            RetryError::Permanent(e) => write!(f, "permanent error: {e}"),
        }
    }
}

impl<E: fmt::Display + fmt::Debug> std::error::Error for RetryError<E> {}

/// Metadata passed to [`Retry::before_attempt`] hooks.
#[derive(Debug, Clone)]
pub struct BeforeAttemptInfo {
    /// Current attempt number, 1-based.
    pub attempt: u32,
    /// The delay that will be slept if this attempt fails. `None` on the final
    /// attempt (no more retries available).
    pub next_delay: Option<Duration>,
    /// Wall-clock time since the retry loop started.
    pub total_elapsed: Duration,
}

/// Metadata passed to [`Retry::after_attempt`] hooks.
#[derive(Debug, Clone)]
pub struct AfterAttemptInfo {
    /// Current attempt number, 1-based.
    pub attempt: u32,
    /// The delay about to be slept before the next attempt. `None` when the
    /// attempt succeeded, hit a permanent error, or retries are exhausted.
    pub next_delay: Option<Duration>,
    /// Wall-clock time since the retry loop started.
    pub total_elapsed: Duration,
}

/// Retry builder. Created by [`retry()`], configured with hook methods, and
/// executed with [`.call().await`](Retry::call) or just `.await`.
pub struct Retry<F> {
    delays: Vec<Duration>,
    operation: F,
    before_all: Box<dyn FnMut()>,
    after_all: Box<dyn FnMut()>,
    before_attempt: Box<dyn FnMut(&BeforeAttemptInfo)>,
    after_attempt: Box<dyn FnMut(&AfterAttemptInfo)>,
}

/// Create a retry builder.
///
/// `delays` is a slice of millisecond wait times between attempts. The total
/// number of attempts is `delays.len() + 1` — one initial try plus one retry
/// per delay entry. An empty slice means a single attempt with no retries.
///
/// ```rust
/// use tokio_retry_simple::{retry, RetryError};
///
/// # async fn example() {
/// // Up to 4 attempts: try, wait 100ms, try, wait 200ms, try, wait 400ms, try.
/// let result = retry(&[100, 200, 400], || async {
///     Ok::<_, RetryError<&str>>("done")
/// })
/// .call()
/// .await;
/// # }
/// ```
pub fn retry<F>(delays: &[u64], operation: F) -> Retry<F> {
    Retry {
        delays: delays.iter().map(|&ms| Duration::from_millis(ms)).collect(),
        operation,
        before_all: Box::new(|| {}),
        after_all: Box::new(|| {}),
        before_attempt: Box::new(|_| {}),
        after_attempt: Box::new(|_| {}),
    }
}

impl<F> Retry<F> {
    /// Hook called once before the retry loop starts.
    pub fn before_all(mut self, hook: impl FnMut() + 'static) -> Self {
        self.before_all = Box::new(hook);
        self
    }

    /// Hook called once after the retry loop ends (success or failure).
    pub fn after_all(mut self, hook: impl FnMut() + 'static) -> Self {
        self.after_all = Box::new(hook);
        self
    }

    /// Hook called before each attempt. Useful for logging.
    ///
    /// ```rust
    /// # use tokio_retry_simple::{retry, RetryError};
    /// # async fn example() {
    /// retry(&[1000], || async { Ok::<_, RetryError<()>>(()) })
    ///     .before_attempt(|info| {
    ///         println!("attempt {}, elapsed {:?}", info.attempt, info.total_elapsed);
    ///     })
    ///     .call()
    ///     .await;
    /// # }
    /// ```
    pub fn before_attempt(mut self, hook: impl FnMut(&BeforeAttemptInfo) + 'static) -> Self {
        self.before_attempt = Box::new(hook);
        self
    }

    /// Hook called after each attempt. `next_delay` is `Some` when a transient
    /// failure is about to be retried, `None` otherwise.
    pub fn after_attempt(mut self, hook: impl FnMut(&AfterAttemptInfo) + 'static) -> Self {
        self.after_attempt = Box::new(hook);
        self
    }
}

impl<F, Fut, T, E> Retry<F>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, RetryError<E>>>,
{
    /// Run the retry loop. Returns `Ok(T)` on success or `Err(E)` on permanent
    /// failure / exhaustion.
    ///
    /// Prefer this over `.await` — it doesn't require `'static` bounds, so your
    /// closure can borrow from the surrounding scope.
    pub async fn call(mut self) -> Result<T, E> {
        (self.before_all)();

        let start = Instant::now();
        let total_attempts = self.delays.len() + 1;
        let mut last_error: Option<E> = None;

        for attempt_idx in 0..total_attempts {
            let attempt = (attempt_idx as u32) + 1;
            let scheduled_delay = self.delays.get(attempt_idx).copied();

            (self.before_attempt)(&BeforeAttemptInfo {
                attempt,
                next_delay: scheduled_delay,
                total_elapsed: start.elapsed(),
            });

            match (self.operation)().await {
                Ok(val) => {
                    (self.after_attempt)(&AfterAttemptInfo {
                        attempt,
                        next_delay: None,
                        total_elapsed: start.elapsed(),
                    });
                    (self.after_all)();
                    return Ok(val);
                }
                Err(RetryError::Permanent(e)) => {
                    (self.after_attempt)(&AfterAttemptInfo {
                        attempt,
                        next_delay: None,
                        total_elapsed: start.elapsed(),
                    });
                    (self.after_all)();
                    return Err(e);
                }
                Err(RetryError::Transient(e)) => {
                    (self.after_attempt)(&AfterAttemptInfo {
                        attempt,
                        next_delay: scheduled_delay,
                        total_elapsed: start.elapsed(),
                    });

                    if let Some(delay) = scheduled_delay {
                        last_error = Some(e);
                        tokio::time::sleep(delay).await;
                    } else {
                        (self.after_all)();
                        return Err(e);
                    }
                }
            }
        }

        (self.after_all)();
        Err(last_error.expect("at least one attempt must have been made"))
    }
}

/// `IntoFuture` lets you `.await` the builder directly as a convenience.
/// Requires `'static` types since the future is boxed. If your closure
/// captures references, use [`.call().await`](Retry::call) instead.
impl<F, Fut, T, E> IntoFuture for Retry<F>
where
    F: FnMut() -> Fut + 'static,
    Fut: Future<Output = Result<T, RetryError<E>>> + 'static,
    T: 'static,
    E: 'static,
{
    type Output = Result<T, E>;
    type IntoFuture = Pin<Box<dyn Future<Output = Result<T, E>>>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.call())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test(start_paused = true)]
    async fn success_on_first_attempt() {
        let result: Result<&str, &str> = retry(&[1000, 2000], || async { Ok("nice") }).call().await;
        assert_eq!(result, Ok("nice"));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_then_succeed() {
        let count = Cell::new(0u32);
        let result = retry(&[100, 200, 400], || {
            let attempt = count.get() + 1;
            count.set(attempt);
            async move {
                if attempt < 3 {
                    Err(RetryError::Transient("not yet"))
                } else {
                    Ok("got it")
                }
            }
        })
        .call()
        .await;

        assert_eq!(result, Ok("got it"));
        assert_eq!(count.get(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn all_retries_exhausted() {
        let count = Cell::new(0u32);
        let result: Result<(), &str> = retry(&[100, 200], || {
            count.set(count.get() + 1);
            async { Err(RetryError::Transient("still broken")) }
        })
        .call()
        .await;

        assert_eq!(result, Err("still broken"));
        assert_eq!(count.get(), 3); // 2 delays + 1 = 3 attempts
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_error_stops_early() {
        let count = Cell::new(0u32);
        let result: Result<(), &str> = retry(&[100, 200, 400], || {
            let attempt = count.get() + 1;
            count.set(attempt);
            async move {
                if attempt == 2 {
                    Err(RetryError::Permanent("fatal"))
                } else {
                    Err(RetryError::Transient("meh"))
                }
            }
        })
        .call()
        .await;

        assert_eq!(result, Err("fatal"));
        assert_eq!(count.get(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn empty_delays_single_attempt() {
        let count = Cell::new(0u32);
        let result: Result<(), &str> = retry(&[], || {
            count.set(count.get() + 1);
            async { Err(RetryError::Transient("fail")) }
        })
        .call()
        .await;

        assert_eq!(result, Err("fail"));
        assert_eq!(count.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn hooks_receive_correct_info() {
        let before_attempts = Rc::new(Cell::new(Vec::new()));
        let after_attempts = Rc::new(Cell::new(Vec::new()));
        let count = Cell::new(0u32);

        let ba = before_attempts.clone();
        let aa = after_attempts.clone();
        let _ = retry(&[100, 200], || {
            let attempt = count.get() + 1;
            count.set(attempt);
            async move {
                if attempt < 3 {
                    Err::<(), _>(RetryError::Transient("nope"))
                } else {
                    Ok(())
                }
            }
        })
        .before_attempt(move |info| {
            let mut v = ba.take();
            v.push((info.attempt, info.next_delay));
            ba.set(v);
        })
        .after_attempt(move |info| {
            let mut v = aa.take();
            v.push((info.attempt, info.next_delay));
            aa.set(v);
        })
        .call()
        .await;

        let before = before_attempts.take();
        assert_eq!(before.len(), 3);
        assert_eq!(before[0], (1, Some(Duration::from_millis(100))));
        assert_eq!(before[1], (2, Some(Duration::from_millis(200))));
        assert_eq!(before[2], (3, None)); // last attempt, no delay

        let after = after_attempts.take();
        assert_eq!(after.len(), 3);
        assert_eq!(after[0], (1, Some(Duration::from_millis(100)))); // transient, will sleep 100ms
        assert_eq!(after[1], (2, Some(Duration::from_millis(200)))); // transient, will sleep 200ms
        assert_eq!(after[2], (3, None)); // succeeded, no delay
    }

    #[tokio::test(start_paused = true)]
    async fn before_after_hook_ordering() {
        let log = Rc::new(Cell::new(Vec::new()));

        let log_op = log.clone();
        let log_ba = log.clone();
        let log_aa = log.clone();
        let log_ball = log.clone();
        let log_aall = log.clone();
        let _ = retry(&[100], move || {
            let attempt = {
                let mut v = log_op.take();
                let a = v.iter().filter(|s: &&String| s.starts_with("op")).count() as u32 + 1;
                v.push(format!("op{a}"));
                log_op.set(v);
                a
            };
            async move {
                if attempt < 2 {
                    Err::<(), _>(RetryError::Transient("err"))
                } else {
                    Ok(())
                }
            }
        })
        .before_all(move || {
            let mut v = log_ball.take();
            v.push("before_all".to_string());
            log_ball.set(v);
        })
        .before_attempt(move |info| {
            let mut v = log_ba.take();
            v.push(format!("before{}", info.attempt));
            log_ba.set(v);
        })
        .after_attempt(move |info| {
            let mut v = log_aa.take();
            v.push(format!("after{}", info.attempt));
            log_aa.set(v);
        })
        .after_all(move || {
            let mut v = log_aall.take();
            v.push("after_all".to_string());
            log_aall.set(v);
        })
        .call()
        .await;

        let entries = log.take();
        assert_eq!(
            entries,
            vec![
                "before_all",
                "before1",
                "op1",
                "after1",
                "before2",
                "op2",
                "after2",
                "after_all",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn builder_ergonomics_with_await() {
        let count = AtomicU32::new(0);
        let result = retry(&[100], move || {
            let attempt = count.fetch_add(1, Ordering::Relaxed) + 1;
            async move {
                if attempt < 2 {
                    Err::<&str, _>(RetryError::Transient("nah"))
                } else {
                    Ok("yep")
                }
            }
        })
        .before_attempt(|_| {})
        .after_attempt(|_| {})
        .await;

        assert_eq!(result, Ok("yep"));
    }

    #[tokio::test(start_paused = true)]
    async fn elapsed_time_advances_with_delays() {
        let elapsed_log = Rc::new(Cell::new(Vec::new()));

        let el = elapsed_log.clone();
        let _ = retry(&[1000, 2000], || async {
            Err::<(), _>(RetryError::Transient("fail"))
        })
        .before_attempt(move |info| {
            let mut v = el.take();
            v.push(info.total_elapsed);
            el.set(v);
        })
        .call()
        .await;

        let elapsed = elapsed_log.take();
        assert_eq!(elapsed.len(), 3);
        assert!(elapsed[0] < Duration::from_millis(10));
        assert!(elapsed[1] >= Duration::from_millis(1000));
        assert!(elapsed[2] >= Duration::from_millis(3000));
    }

    #[tokio::test(start_paused = true)]
    async fn before_all_after_all_called_on_success() {
        let log = Rc::new(Cell::new(Vec::<&str>::new()));

        let l1 = log.clone();
        let l2 = log.clone();
        let _ = retry(&[100], || async { Ok::<_, RetryError<()>>(()) })
            .before_all(move || {
                let mut v = l1.take();
                v.push("before_all");
                l1.set(v);
            })
            .after_all(move || {
                let mut v = l2.take();
                v.push("after_all");
                l2.set(v);
            })
            .call()
            .await;

        assert_eq!(log.take(), vec!["before_all", "after_all"]);
    }

    #[tokio::test(start_paused = true)]
    async fn before_all_after_all_called_on_permanent() {
        let log = Rc::new(Cell::new(Vec::<&str>::new()));

        let l1 = log.clone();
        let l2 = log.clone();
        let _: Result<(), &str> = retry(&[100], || async {
            Err(RetryError::Permanent("boom"))
        })
        .before_all(move || {
            let mut v = l1.take();
            v.push("before_all");
            l1.set(v);
        })
        .after_all(move || {
            let mut v = l2.take();
            v.push("after_all");
            l2.set(v);
        })
        .call()
        .await;

        assert_eq!(log.take(), vec!["before_all", "after_all"]);
    }

    #[tokio::test(start_paused = true)]
    async fn before_all_after_all_called_on_exhaustion() {
        let log = Rc::new(Cell::new(Vec::<&str>::new()));

        let l1 = log.clone();
        let l2 = log.clone();
        let _: Result<(), &str> = retry(&[], || async {
            Err(RetryError::Transient("nope"))
        })
        .before_all(move || {
            let mut v = l1.take();
            v.push("before_all");
            l1.set(v);
        })
        .after_all(move || {
            let mut v = l2.take();
            v.push("after_all");
            l2.set(v);
        })
        .call()
        .await;

        assert_eq!(log.take(), vec!["before_all", "after_all"]);
    }
}
