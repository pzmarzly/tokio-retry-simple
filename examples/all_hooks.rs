use std::sync::atomic::{AtomicU32, Ordering};

use tokio_retry_simple::{retry, RetryError};

static ATTEMPT: AtomicU32 = AtomicU32::new(0);

async fn flaky_request() -> Result<String, String> {
    let n = ATTEMPT.fetch_add(1, Ordering::Relaxed) + 1;
    if n < 4 {
        Err(format!("connection refused (attempt {n})"))
    } else {
        Ok(format!("response payload (attempt {n})"))
    }
}

#[tokio::main]
async fn main() {
    let result = retry(&[500, 1000, 2000], || async {
        flaky_request().await.map_err(RetryError::Transient)
    })
    .before_all(|info| {
        println!("--- starting flaky_request (attempt {}) ---", info.attempt);
    })
    .before_attempt(|info| {
        println!(
            "[attempt {}] firing (elapsed: {:?})",
            info.attempt, info.total_elapsed
        );
    })
    .after_attempt(|info| {
        if info.success {
            println!("[attempt {}] succeeded!", info.attempt);
        } else if let Some(delay) = info.next_delay {
            println!("[attempt {}] failed, sleeping {delay:?}...", info.attempt);
        } else {
            println!("[attempt {}] failed (no retries left)", info.attempt);
        }
    })
    .after_all(|info| {
        println!(
            "--- finished: {} after {} attempt(s), took {:?} ---",
            if info.success { "success" } else { "failure" },
            info.attempt,
            info.total_elapsed
        );
    })
    .call()
    .await;

    match result {
        Ok(val) => println!("result: {val}"),
        Err(e) => println!("gave up: {e}"),
    }
}
