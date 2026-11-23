//! Raw HTTP to the Messages API: headers, retries with backoff.

use std::time::Duration;

use serde_json::Value;

use crate::key::ApiKey;

pub const API_VERSION: &str = "2026-10-21";

/// When to retry a failed request.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Retries after the first attempt.
    pub max_retries: u32,
    /// Delay before the first retry; doubles each time (with jitter).
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 4,
            base_delay: Duration::from_millis(1000),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// Delay before retry number `attempt` (0-based), honoring `retry-after`.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(d) = retry_after {
            return d.min(Duration::from_secs(120));
        }
        let exp = self.base_delay.saturating_mul(1u32 << attempt.min(16));
        let capped = exp.min(self.max_delay);
        // Up to 25% jitter, from the clock (no RNG dependency needed).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let jitter = capped.mul_f64(f64::from(nanos % 1000) / 4000.0);
        capped + jitter
    }
}

/// An error response of the API (or a transport failure, `status == 0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub kind: String,
    pub message: String,
    pub retry_after: Option<Duration>,
    pub retryable: bool,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hint = match self.status {
            401 => " Check the Anthropic API key.",
            404 if self.message.starts_with("model:") => " Check the model ID.",
            _ => "",
        };
        if self.status == 0 {
            write!(f, "could not reach the Anthropic API: {}", self.message)
        } else {
            write!(
                f,
                "Anthropic API error {} ({}): {}{hint}",
                self.status, self.kind, self.message
            )
        }
    }
}

/// Classifies an error response.
pub fn classify(status: u16, retry_after: Option<Duration>, body: &str) -> ApiError {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let kind = v
        .pointer("/error/type")
        .and_then(Value::as_str)
        .unwrap_or("http_error")
        .to_string();
    let message = v
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(300).collect());
    let spend_cap = v
        .pointer("/error/details/error_code")
        .and_then(Value::as_str)
        == Some("enforced_spend_limit_reached");
    let retryable = match status {
        408 | 409 => true,
        429 => !spend_cap,
        s => s >= 500,
    };
    ApiError {
        status,
        kind,
        message,
        retry_after,
        retryable,
    }
}

fn parse_retry_after(h: &reqwest::header::HeaderMap) -> Option<Duration> {
    let v = h.get("retry-after")?.to_str().ok()?.trim();
    v.parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(Duration::from_secs_f64)
}

#[derive(Clone)]
pub struct HttpClient {
    client: reqwest::Client,
    url: String,
    key: ApiKey,
    betas: Vec<String>,
    pub retry: RetryPolicy,
}

impl HttpClient {
    pub fn new(
        base_url: &str,
        key: ApiKey,
        betas: Vec<String>,
        retry: RetryPolicy,
    ) -> Result<Self, String> {
        // reqwest is built without a default crypto provider; use ring.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| format!("could not create the HTTP client: {e}"))?;
        Ok(Self {
            client,
            url: format!("{}/v1/messages", base_url.trim_end_matches('/')),
            key,
            betas,
            retry,
        })
    }

    /// One POST attempt; a non-2xx status becomes an [`ApiError`].
    pub async fn post(&self, body: bytes::Bytes) -> Result<reqwest::Response, ApiError> {
        let mut req = self
            .client
            .post(&self.url)
            .header("x-api-key", self.key.expose())
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(body);
        if !self.betas.is_empty() {
            req = req.header("anthropic-beta", self.betas.join(","));
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => Ok(resp),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let retry_after = parse_retry_after(resp.headers());
                let body = resp.text().await.unwrap_or_default();
                Err(classify(status, retry_after, &body))
            }
            Err(e) => Err(ApiError {
                status: 0,
                kind: "connection_error".into(),
                // reqwest errors carry the URL, never headers.
                message: e.to_string(),
                retry_after: None,
                retryable: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert!(
            classify(
                429,
                None,
                r#"{"error":{"type":"rate_limit_error","message":"slow"}}"#
            )
            .retryable
        );
        assert!(!classify(429, None, r#"{"error":{"type":"rate_limit_error","message":"cap","details":{"error_code":"enforced_spend_limit_reached"}}}"#).retryable);
        assert!(
            classify(
                529,
                None,
                r#"{"error":{"type":"overloaded_error","message":"o"}}"#
            )
            .retryable
        );
        assert!(classify(500, None, "oops").retryable);
        for s in [400, 401, 403, 404, 413] {
            assert!(!classify(s, None, "{}").retryable, "{s}");
        }
        let e = classify(
            401,
            None,
            r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        );
        assert_eq!(e.kind, "authentication_error");
        assert!(e.to_string().contains("API key"));
    }

    #[test]
    fn delays() {
        let p = RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(250),
        };
        assert!(
            p.delay(0, None) >= Duration::from_millis(100)
                && p.delay(0, None) < Duration::from_millis(126)
        );
        assert!(p.delay(5, None) <= Duration::from_millis(313));
        assert_eq!(
            p.delay(0, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
    }
}
