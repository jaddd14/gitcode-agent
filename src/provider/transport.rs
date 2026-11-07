//! HTTP transport validates a whole turn before exposing executable tool calls.

use super::{
    anthropic,
    canonical::{Event, Request},
    retry::Policy,
    sse::Decoder,
};
use crate::credentials::Secret;
use futures_util::StreamExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// Cancellation-safe Messages client; dropping its request future closes the stream.
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    secret: Option<Secret>,
    bearer: bool,
    timeout: Duration,
    retry: Policy,
}

impl Client {
    /// Construct an endpoint client without making a network call.
    pub fn new(
        base_url: &str,
        secret: Option<Secret>,
        bearer: bool,
        timeout: Duration,
    ) -> Result<Self, String> {
        let url = reqwest::Url::parse(base_url).map_err(|_| "invalid provider URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("provider URL must use HTTP(S) without embedded credentials".into());
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot initialize HTTP client")?;
        Ok(Self {
            http,
            endpoint: format!("{}/v1/messages", base_url.trim_end_matches('/')),
            secret,
            bearer,
            timeout,
            retry: Policy::default(),
        })
    }

    /// Stream non-executable progress immediately and return validated final events.
    pub async fn run(
        &self,
        request: &Request,
        progress: Option<&mpsc::Sender<Event>>,
    ) -> Result<Vec<Event>, String> {
        tokio::time::timeout(self.timeout, self.run_inner(request, progress))
            .await
            .map_err(|_| "provider request timed out".to_string())?
    }

    async fn run_inner(
        &self,
        request: &Request,
        progress: Option<&mpsc::Sender<Event>>,
    ) -> Result<Vec<Event>, String> {
        let body = anthropic::encode(request)?;
        for attempt in 0..=self.retry.retries {
            let mut builder = self
                .http
                .post(&self.endpoint)
                .header("anthropic-version", "2023-06-01")
                .json(&body);
            if let Some(secret) = &self.secret {
                builder = if self.bearer {
                    builder.bearer_auth(secret.expose())
                } else {
                    builder.header("x-api-key", secret.expose())
                };
            }
            let response = builder
                .send()
                .await
                .map_err(|_| "provider connection failed")?;
            let status = response.status();
            if !status.is_success() {
                let after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs);
                let jitter = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as u64;
                if let Some(delay) = self.retry.delay(attempt, status.as_u16(), after, jitter) {
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(format!("provider HTTP status {}", status.as_u16()));
            }
            let mut bytes = response.bytes_stream();
            let mut decoder = Decoder::default();
            let mut stream = anthropic::Stream::default();
            let mut events = Vec::new();
            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.map_err(|_| "provider stream disconnected")?;
                for record in decoder.push(&chunk)? {
                    for event in stream.accept(record)? {
                        if matches!(
                            event,
                            Event::TextDelta { .. }
                                | Event::ThinkingDelta { .. }
                                | Event::Usage { .. }
                        ) {
                            if let Some(sender) = progress {
                                let _ = sender.send(event.clone()).await;
                            }
                        }
                        events.push(event);
                    }
                }
            }
            decoder.finish()?;
            stream.finish()?;
            return Ok(events);
        }
        Err("retry budget exhausted".into())
    }
}
