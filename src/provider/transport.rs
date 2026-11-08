//! HTTP transport validates a whole turn before exposing executable tool calls.

use super::{
    anthropic,
    canonical::{Event, Request},
    retry::{retry_after, Policy},
    sse::Decoder,
};
use super::{openai_chat, openai_responses};
use crate::{config::Protocol, credentials::Secret};
use futures_util::StreamExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

enum WireStream {
    Anthropic(anthropic::Stream),
    Chat(openai_chat::Stream),
    Responses(openai_responses::Stream),
}

impl WireStream {
    fn new(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Anthropic => Self::Anthropic(anthropic::Stream::default()),
            Protocol::OpenaiChat => Self::Chat(openai_chat::Stream::default()),
            Protocol::OpenaiResponses => Self::Responses(openai_responses::Stream::default()),
        }
    }
    fn accept(&mut self, record: super::sse::Record) -> Result<Vec<Event>, String> {
        match self {
            Self::Anthropic(s) => s.accept(record),
            Self::Chat(s) => s.accept(record),
            Self::Responses(s) => s.accept(record),
        }
    }
    fn finish(&self) -> Result<(), String> {
        match self {
            Self::Anthropic(s) => s.finish(),
            Self::Chat(s) => s.finish(),
            Self::Responses(s) => s.finish(),
        }
    }
}

/// Cancellation-safe Messages client; dropping its request future closes the stream.
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    secret: Option<Secret>,
    bearer: bool,
    timeout: Duration,
    retry: Policy,
    protocol: Protocol,
}

impl Client {
    /// Construct an endpoint client without making a network call.
    pub fn new(
        base_url: &str,
        secret: Option<Secret>,
        bearer: bool,
        timeout: Duration,
    ) -> Result<Self, String> {
        Self::with_protocol(base_url, secret, bearer, timeout, Protocol::Anthropic)
    }

    /// Construct a client for any supported protocol without network activity.
    pub fn with_protocol(
        base_url: &str,
        secret: Option<Secret>,
        bearer: bool,
        timeout: Duration,
        protocol: Protocol,
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
            endpoint: format!(
                "{}{}",
                base_url.trim_end_matches('/'),
                match protocol {
                    Protocol::Anthropic => "/v1/messages",
                    Protocol::OpenaiChat => "/chat/completions",
                    Protocol::OpenaiResponses => "/responses",
                }
            ),
            secret,
            bearer,
            timeout,
            retry: Policy::default(),
            protocol,
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
        let body = match self.protocol {
            Protocol::Anthropic => anthropic::encode(request)?,
            Protocol::OpenaiChat => openai_chat::encode(request)?,
            Protocol::OpenaiResponses => openai_responses::encode(request)?,
        };
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
            let response = match builder.send().await {
                Ok(response) => response,
                Err(error) => {
                    // Builder errors are deterministic (for example invalid auth headers).
                    if !error.is_builder() && self.pause(attempt, 503, None).await {
                        continue;
                    }
                    return Err("provider connection failed".into());
                }
            };
            let status = response.status();
            if !status.is_success() {
                let after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|value| retry_after(value, SystemTime::now()));
                // Do not retain a failed response connection during backoff.
                drop(response);
                if self.pause(attempt, status.as_u16(), after).await {
                    continue;
                }
                return Err(format!("provider HTTP status {}", status.as_u16()));
            }
            // Existing consumers append deltas and have no reset event. Replayed
            // attempts must therefore stay silent; their validated final result
            // replaces the provisional first-attempt display at the caller.
            let live = if attempt == 0 { progress } else { None };
            match self.read_stream(response, live).await {
                Ok(events) => return Ok(events),
                Err((message, transient)) => {
                    if transient && self.pause(attempt, 503, None).await {
                        continue;
                    }
                    return Err(message);
                }
            }
        }
        Err("retry budget exhausted".into())
    }

    async fn pause(&self, attempt: u32, status: u16, after: Option<Duration>) -> bool {
        let jitter = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;
        if let Some(delay) = self.retry.delay(attempt, status, after, jitter) {
            tokio::time::sleep(delay).await;
            true
        } else {
            false
        }
    }

    async fn read_stream(
        &self,
        response: reqwest::Response,
        progress: Option<&mpsc::Sender<Event>>,
    ) -> Result<Vec<Event>, (String, bool)> {
        let mut bytes = response.bytes_stream();
        let mut decoder = Decoder::default();
        let mut stream = WireStream::new(self.protocol);
        let mut events = Vec::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|_| ("provider stream disconnected".into(), true))?;
            for record in decoder.push(&chunk).map_err(|error| (error, false))? {
                for event in stream.accept(record).map_err(|error| (error, false))? {
                    if matches!(
                        event,
                        Event::TextDelta { .. } | Event::ThinkingDelta { .. } | Event::Usage { .. }
                    ) {
                        if let Some(sender) = progress {
                            let _ = sender.send(event.clone()).await;
                        }
                    }
                    events.push(event);
                }
            }
        }
        decoder.finish().map_err(|error| (error, true))?;
        stream.finish().map_err(|error| (error, true))?;
        Ok(events)
    }
}
