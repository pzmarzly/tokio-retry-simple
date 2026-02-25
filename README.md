# tokio_retry_simple

`tokio_retry` but simple.

No strategy objects, no iterator-based delay generators, no traits. Just pass your
delays as a slice and go.

Most retry scenarios look like this: try up to N times with hardcoded backoff durations.
Libraries that model delays as composable iterator chains are clever, but in practice you
almost always end up with a static config anyway. This crate skips the abstraction and gives
you a plain `&[u64]`.

## Usage

```rust
use tokio_retry_simple::{retry, RetryError};

let result = retry(&[1000, 2000, 4000], || async {
    match do_something().await {
        Ok(val) => Ok(val),
        Err(e) if e.is_transient() => Err(RetryError::Transient(e)),
        Err(e) => Err(RetryError::Permanent(e)),
    }
})
.call()
.await;
```

The delays slice is milliseconds. `&[1000, 2000, 4000]` means up to 4 attempts: try once,
wait 1s, try again, wait 2s, try again, wait 4s, try one last time.

## Error classification

Your closure returns `Result<T, RetryError<E>>`:

- **`RetryError::Transient(e)`** — retryable, will try again if attempts remain
- **`RetryError::Permanent(e)`** — fatal, stops immediately

The final result is `Result<T, E>` — the `RetryError` wrapper is stripped off.

```rust
use tokio_retry_simple::{retry, RetryError};

// works naturally with the ? operator
let val = retry(&[500, 1000], || async {
    let conn = connect().await.map_err(RetryError::Transient)?;
    let data = conn.query().await.map_err(RetryError::Permanent)?;
    Ok(data)
})
.call()
.await?;
```

## Hooks

Four optional hooks for logging or metrics:

| Hook | Called | Receives |
|------|--------|----------|
| `before_all` | Once, before the first attempt | nothing |
| `before_attempt` | Before each attempt | `BeforeAttemptInfo` |
| `after_attempt` | After each attempt | `AfterAttemptInfo` |
| `after_all` | Once, after the last attempt | nothing |

```rust
use tokio_retry_simple::{retry, RetryError};

let result = retry(&[1000, 2000], || async {
    fetch_data().await.map_err(RetryError::Transient)
})
.before_all(|| println!("starting request"))
.before_attempt(|info| {
    println!("attempt {} (elapsed: {:?})", info.attempt, info.total_elapsed);
})
.after_attempt(|info| {
    if let Some(delay) = info.next_delay {
        println!("  failed, retrying in {:?}", delay);
    }
})
.after_all(|| println!("done"))
.call()
.await;
```

`BeforeAttemptInfo` and `AfterAttemptInfo` both carry `attempt`, `next_delay`, and
`total_elapsed`. The difference is in `next_delay` semantics:

- **before**: the delay that *will* be slept if this attempt fails (`None` on last attempt)
- **after**: the delay *about to* be slept (`None` if succeeded, permanent, or exhausted)

## `.call()` vs `.await`

Two ways to run:

- **`.call().await`** — no `'static` bounds required, works with closures that borrow from the environment
- **`.await`** (via `IntoFuture`) — convenience shorthand, requires `'static` types

Prefer `.call().await` when your closure captures references.

## Common delay patterns

```rust
// Fixed interval
retry(&[1000, 1000, 1000], || async { /* ... */ })

// Exponential backoff
retry(&[100, 200, 400, 800, 1600], || async { /* ... */ })

// Single attempt, no retries
retry(&[], || async { /* ... */ })

// Quick retries then give up
retry(&[50, 50, 50], || async { /* ... */ })
```

It's just a slice. Compute it however you want:

```rust
// Generate exponential backoff programmatically
let delays: Vec<u64> = (0..5).map(|i| 100 * 2u64.pow(i)).collect();
retry(&delays, || async { /* ... */ }).call().await;
```
