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
    .before_all(|| {
        println!("--- starting flaky_request with up to 4 attempts ---");
    })
    .before_attempt(|info| {
        print!("[attempt {}] firing", info.attempt);
        if let Some(delay) = info.next_delay {
            println!(" (will wait {delay:?} on failure)");
        } else {
            println!(" (last chance)");
        }
    })
    .after_attempt(|info| {
        if let Some(delay) = info.next_delay {
            println!("[attempt {}] failed, sleeping {delay:?}...", info.attempt);
        } else {
            println!(
                "[attempt {}] done (elapsed: {:?})",
                info.attempt, info.total_elapsed
            );
        }
    })
    .after_all(|| {
        println!("--- finished ---");
    })
    .call()
    .await;

    match result {
        Ok(val) => println!("success: {val}"),
        Err(e) => println!("gave up: {e}"),
    }
}
