//! Bounded retry policy for idempotent metadata and download requests.
//!
//! Transient failures (timeouts, connection errors, 408/425/429, and 5xx) are
//! retried with exponential backoff. `Retry-After` in seconds is honored, and
//! every wait is capped so a stale header cannot stall a launcher operation.

use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Response, StatusCode};
use std::time::Duration;

/// Longest accepted single wait.
pub const MAX_BACKOFF: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(250),
            max_delay: MAX_BACKOFF,
        }
    }
}

impl RetryPolicy {
    pub fn attempts(&self) -> u32 {
        self.max_attempts.max(1)
    }

    /// Wait after a 1-based attempt. A present `Retry-After` wins, but is
    /// clamped to `max_delay`; otherwise the delay doubles from `base_delay`.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(delay) = retry_after {
            return delay.min(self.max_delay);
        }
        let exponent = attempt.saturating_sub(1).min(16);
        (self.base_delay * 2_u32.saturating_pow(exponent)).min(self.max_delay)
    }
}

/// Statuses worth retrying for a bodyless GET. 501/505 are included through
/// `is_server_error`; the cost is one bounded retry, not a loop.
pub fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429) || status.is_server_error()
}

pub fn retryable_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect()
}

/// Parse the seconds form of `Retry-After`. The HTTP-date form is ignored and
/// falls back to exponential backoff rather than requiring a date parser.
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

/// Send a GET, rebuilding the request for each attempt. The final response is
/// returned even when it is still retryable, so callers keep their existing
/// `error_for_status()` semantics.
pub async fn get_with_retry<F>(
    policy: &RetryPolicy,
    mut build: F,
) -> Result<Response, reqwest::Error>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let attempts = policy.attempts();
    let mut attempt = 0;
    loop {
        attempt += 1;
        match build().send().await {
            Ok(response) => {
                let wait = retry_after(response.headers());
                if attempt < attempts && retryable_status(response.status()) {
                    drop(response);
                    tokio::time::sleep(policy.delay(attempt, wait)).await;
                    continue;
                }
                return Ok(response);
            }
            Err(error) => {
                if attempt < attempts && retryable_error(&error) {
                    tokio::time::sleep(policy.delay(attempt, None)).await;
                    continue;
                }
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_BACKOFF, RetryPolicy, get_with_retry, retry_after, retryable_status};
    use reqwest::StatusCode;
    use reqwest::header::{HeaderMap, RETRY_AFTER};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn backs_off_exponentially_up_to_the_cap() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay(1, None), Duration::from_millis(250));
        assert_eq!(policy.delay(2, None), Duration::from_millis(500));
        assert_eq!(policy.delay(4, None), Duration::from_secs(2));
        assert_eq!(policy.delay(20, None), MAX_BACKOFF);
    }

    #[test]
    fn retry_after_wins_but_never_exceeds_the_cap() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.delay(1, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(policy.delay(1, Some(Duration::from_secs(60))), MAX_BACKOFF);
        assert_eq!(policy.delay(1, Some(Duration::ZERO)), Duration::ZERO);
    }

    #[test]
    fn parses_seconds_retry_after_and_ignores_dates_and_garbage() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(2)));
        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
        headers.insert(RETRY_AFTER, "later".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn retries_transient_statuses_only() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(retryable_status(status), "{status} should retry");
        }
        for status in [
            StatusCode::OK,
            StatusCode::NOT_FOUND,
            StatusCode::FORBIDDEN,
            StatusCode::BAD_REQUEST,
        ] {
            assert!(!retryable_status(status), "{status} should not retry");
        }
    }

    async fn respond(socket: &mut tokio::net::TcpStream, response: &str) {
        let mut request = [0_u8; 2048];
        let _ = socket.read(&mut request).await;
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retries_a_429_then_returns_the_successful_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            respond(
                &mut first,
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await;
            let (mut second, _) = listener.accept().await.unwrap();
            respond(
                &mut second,
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            )
            .await;
        });
        let client = reqwest::Client::new();
        let url = format!("http://{address}/resource");
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
        };
        let response = get_with_retry(&policy, || client.get(&url)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn returns_the_last_retryable_response_after_the_attempt_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let server_hits = Arc::clone(&hits);
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                server_hits.fetch_add(1, Ordering::Relaxed);
                respond(
                    &mut socket,
                    "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            }
        });
        let client = reqwest::Client::new();
        let url = format!("http://{address}/resource");
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
        };
        let response = get_with_retry(&policy, || client.get(&url)).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(hits.load(Ordering::Relaxed), 3);
        server.await.unwrap();
    }
}
