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
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    secret: Option<Secret>,
    bearer: bool,
    timeout: Duration,
    retry: Policy,
    protocol: Protocol,
    effort: Option<String>,
    prompt_cache: bool,
    cache_key: Option<String>,
    official_host: Option<String>,
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
        let official_host = if url.scheme() == "https" && url.port_or_known_default() == Some(443) {
            url.host_str().map(str::to_owned)
        } else {
            None
        };
        let mut nonce = [0_u8; 16];
        let cache_key = getrandom::fill(&mut nonce)
            .ok()
            .map(|()| nonce.iter().map(|b| format!("{b:02x}")).collect::<String>());
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
            effort: None,
            prompt_cache: true,
            cache_key,
            official_host,
        })
    }

    /// Enable or disable cache hints on supported official endpoints.
    pub fn set_prompt_cache(&mut self, enabled: bool) {
        self.prompt_cache = enabled;
    }

    /// Set the requested reasoning effort for subsequent requests; `None` uses the provider default.
    pub fn set_effort(&mut self, effort: Option<&str>) -> Result<(), &'static str> {
        if effort.is_some_and(|value| !matches!(value, "low" | "medium" | "high" | "xhigh" | "max"))
        {
            return Err("unknown effort");
        }
        self.effort = effort.map(str::to_owned);
        Ok(())
    }

    /// Currently selected reasoning effort, if explicitly set.
    pub fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    /// Fetch every model advertised for the current endpoint and credential.
    pub async fn list_models(&self) -> Result<Vec<String>, String> {
        let prefix = match self.protocol {
            Protocol::Anthropic => self.endpoint.strip_suffix("/messages"),
            Protocol::OpenaiChat => self.endpoint.strip_suffix("/chat/completions"),
            Protocol::OpenaiResponses => self.endpoint.strip_suffix("/responses"),
        }
        .ok_or("invalid provider endpoint")?;
        let prefix = if !prefix.contains("/v1")
            && reqwest::Url::parse(prefix)
                .map_err(|_| "invalid provider endpoint")?
                .path()
                .trim_matches('/')
                .is_empty()
        {
            format!("{prefix}/v1")
        } else {
            prefix.to_owned()
        };
        let initial = reqwest::Url::parse(&format!("{prefix}/models"))
            .map_err(|_| "invalid models endpoint")?;
        let mut next = initial.clone();
        let mut visited = std::collections::HashSet::new();
        let mut known = std::collections::HashSet::new();
        let mut models = Vec::new();
        let mut bearer = self.bearer;
        loop {
            if visited.len() >= 1000 || !visited.insert(next.as_str().to_owned()) {
                return Err("pagination модели повторяется или превышен лимит страниц".into());
            }
            let send = |bearer: bool| {
                let mut request = self
                    .http
                    .get(next.clone())
                    .timeout(self.timeout.min(Duration::from_secs(15)))
                    .header("anthropic-version", "2023-06-01");
                if let Some(secret) = &self.secret {
                    request = if bearer {
                        request.bearer_auth(secret.expose())
                    } else {
                        request.header("x-api-key", secret.expose())
                    };
                }
                request.send()
            };
            let mut response = send(bearer)
                .await
                .map_err(|_| "не удалось подключиться к провайдеру")?;
            // Some Anthropic-compatible relays use bearer authentication for /v1/models.
            if !bearer && matches!(response.status().as_u16(), 401 | 403) && self.secret.is_some() {
                bearer = true;
                response = send(true)
                    .await
                    .map_err(|_| "не удалось подключиться к провайдеру")?;
            }
            if !response.status().is_success() {
                return Err(format!(
                    "провайдер вернул HTTP {}",
                    response.status().as_u16()
                ));
            }
            const MAX_RESPONSE: usize = 16 * 1024 * 1024;
            if response
                .content_length()
                .is_some_and(|size| size > MAX_RESPONSE as u64)
            {
                return Err("список моделей слишком велик".into());
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| "ошибка чтения списка моделей")?;
                if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
                    return Err("список моделей слишком велик".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            let json: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| "некорректный список моделей")?;
            let data = json
                .get("data")
                .or_else(|| json.get("models"))
                .and_then(|data| data.as_array())
                .or_else(|| json.as_array())
                .ok_or("провайдер не вернул список моделей")?;
            let mut last = None;
            for entry in data {
                if let Some(id) = entry
                    .as_str()
                    .or_else(|| entry.get("id").and_then(|id| id.as_str()))
                {
                    if !id.trim().is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
                    {
                        last = Some(id.to_owned());
                        if known.insert(id.to_owned()) {
                            models.push(id.to_owned());
                        }
                    }
                }
            }
            if let Some(link) = json
                .get("next")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
            {
                let target = initial
                    .join(link)
                    .map_err(|_| "некорректная ссылка на страницу моделей")?;
                if target.origin() != initial.origin()
                    || target.path() != initial.path()
                    || !target.username().is_empty()
                    || target.password().is_some()
                {
                    return Err("небезопасная ссылка на страницу моделей".into());
                }
                next = target;
            } else if let Some(token) = json
                .get("next_page_token")
                .or_else(|| json.get("nextPageToken"))
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
            {
                next = initial.clone();
                next.query_pairs_mut().append_pair("page_token", token);
            } else if json.get("has_more").and_then(|v| v.as_bool()) == Some(true) {
                let cursor = json
                    .get("last_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .or(last)
                    .ok_or("нет курсора следующей страницы моделей")?;
                next = initial.clone();
                next.query_pairs_mut().append_pair("after_id", &cursor);
            } else {
                break;
            }
        }
        Ok(models)
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
        let mut body = match self.protocol {
            Protocol::Anthropic => anthropic::encode(request)?,
            Protocol::OpenaiChat => openai_chat::encode(request)?,
            Protocol::OpenaiResponses => openai_responses::encode(request)?,
        };
        apply_cache_policy(
            &mut body,
            self.protocol,
            self.official_host.as_deref(),
            self.prompt_cache,
            self.cache_key.as_deref(),
        );
        if let Some(effort) = self.effort() {
            match self.protocol {
                Protocol::Anthropic => {
                    body["output_config"] = serde_json::json!({"effort": effort})
                }
                Protocol::OpenaiChat => body["reasoning_effort"] = serde_json::json!(effort),
                Protocol::OpenaiResponses => {
                    body["reasoning"] = serde_json::json!({"effort": effort})
                }
            }
        }
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
                    if !error.is_builder() && self.pause(attempt, 503, None, progress).await {
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
                if self.pause(attempt, status.as_u16(), after, progress).await {
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
                    if transient && self.pause(attempt, 503, None, progress).await {
                        continue;
                    }
                    return Err(message);
                }
            }
        }
        Err("retry budget exhausted".into())
    }

    async fn pause(
        &self,
        attempt: u32,
        status: u16,
        after: Option<Duration>,
        progress: Option<&mpsc::Sender<Event>>,
    ) -> bool {
        let jitter = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;
        if let Some(delay) = self.retry.delay(attempt, status, after, jitter) {
            if let Some(progress) = progress {
                let _ = progress
                    .send(Event::RetryNotice {
                        attempt: attempt.saturating_add(2),
                        max_attempts: self.retry.retries.saturating_add(1),
                        delay_ms: delay.as_millis().min(u128::from(u64::MAX)) as u64,
                    })
                    .await;
            }
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
        fill_missing_output_usage(&mut events);
        Ok(events)
    }
}

// Some compatible providers return zero or omit output usage despite generating
// text. Keep real provider counts when available and mark the fallback as an
// estimate so the UI and usage ledger never present it as an exact count.
fn fill_missing_output_usage(events: &mut Vec<Event>) {
    let characters: u64 = events
        .iter()
        .map(|event| match event {
            Event::TextDelta { text } | Event::ThinkingDelta { text } => {
                text.chars().count() as u64
            }
            Event::ToolCall { name, input, .. } => {
                (name.chars().count() + input.to_string().chars().count()) as u64
            }
            _ => 0,
        })
        .sum();
    let estimate = characters.div_ceil(4);
    if estimate == 0 {
        return;
    }
    if let Some(Event::Usage { usage }) = events
        .iter_mut()
        .rev()
        .find(|event| matches!(event, Event::Usage { .. }))
    {
        if usage.output_tokens == 0 || (usage.estimated && usage.output_tokens < estimate) {
            usage.output_tokens = estimate;
            usage.estimated = true;
        }
    } else {
        let position = events
            .iter()
            .rposition(|event| matches!(event, Event::Done { .. }))
            .unwrap_or(events.len());
        events.insert(
            position,
            Event::Usage {
                usage: super::canonical::Usage {
                    output_tokens: estimate,
                    estimated: true,
                    ..Default::default()
                },
            },
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod usage_tests {
    use super::*;
    use crate::provider::canonical::Usage;
    #[test]
    fn missing_or_zero_usage_is_estimated_but_provider_counts_are_preserved() {
        for reported in [None, Some(0), Some(7)] {
            let mut events = vec![Event::TextDelta {
                text: "Привет!".into(),
            }];
            if let Some(output_tokens) = reported {
                events.push(Event::Usage {
                    usage: Usage {
                        input_tokens: 18,
                        output_tokens,
                        ..Default::default()
                    },
                });
            }
            events.push(Event::Done {
                reason: "end_turn".into(),
            });
            fill_missing_output_usage(&mut events);
            let usage = events
                .iter()
                .rev()
                .find_map(|event| match event {
                    Event::Usage { usage } => Some(usage),
                    _ => None,
                })
                .unwrap();
            assert_eq!(usage.output_tokens, if reported == Some(7) { 7 } else { 2 });
            assert_eq!(usage.estimated, reported != Some(7));
            if reported.is_some() {
                assert_eq!(usage.input_tokens, 18);
            }
            assert!(matches!(events.last(), Some(Event::Done { .. })));
        }
    }
}

fn apply_cache_policy(
    body: &mut serde_json::Value,
    protocol: Protocol,
    host: Option<&str>,
    enabled: bool,
    key: Option<&str>,
) {
    if !enabled {
        return;
    }
    match (protocol, host) {
        (Protocol::Anthropic, Some("api.anthropic.com")) => {
            body["cache_control"] = serde_json::json!({"type":"ephemeral"})
        }
        (Protocol::OpenaiResponses, Some("api.openai.com")) => {
            if let Some(key) = key {
                body["prompt_cache_key"] = serde_json::json!(key);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod cache_policy_tests {
    use super::*;
    #[test]
    fn cache_hints_are_scoped_to_official_hosts_and_can_be_disabled() {
        let mut anthropic = serde_json::json!({});
        apply_cache_policy(
            &mut anthropic,
            Protocol::Anthropic,
            Some("api.anthropic.com"),
            true,
            None,
        );
        assert_eq!(anthropic["cache_control"]["type"], "ephemeral");
        let mut openai = serde_json::json!({});
        apply_cache_policy(
            &mut openai,
            Protocol::OpenaiResponses,
            Some("api.openai.com"),
            true,
            Some("random-session"),
        );
        assert_eq!(openai["prompt_cache_key"], "random-session");
        for (protocol, host, enabled) in [
            (Protocol::Anthropic, "relay.example", true),
            (Protocol::OpenaiResponses, "api.openai.com", false),
            (Protocol::OpenaiChat, "api.openai.com", true),
        ] {
            let mut body = serde_json::json!({});
            apply_cache_policy(&mut body, protocol, Some(host), enabled, Some("x"));
            assert_eq!(body, serde_json::json!({}));
        }
    }
}
