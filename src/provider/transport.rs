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

/// Status used to schedule a retry for a request that hit its deadline.
const TIMEOUT_STATUS: u16 = 408;
/// Message preserved for callers and the UI that already recognize it.
const TIMEOUT_MESSAGE: &str = "provider request timed out";
/// Stable rejection message for a request whose payload exceeds the provider limit.
pub(crate) const TOO_LARGE_MESSAGE: &str = "provider rejected the request payload as too large";
/// Stable rejection message for a conversation the provider refuses as too long.
pub(crate) const TOO_LONG_MESSAGE: &str = "provider rejected the request as too long";

enum WireStream {
    Anthropic(anthropic::Stream),
    Chat(openai_chat::Stream),
    Responses(openai_responses::Stream),
}

/// Distinguish a rejected request that compaction can shrink from an opaque failure.
///
/// A provider that refuses a conversation as too long (or its payload as too large)
/// is a recoverable condition: the caller shrinks the request and sends it again.
/// Returning a stable message keeps that decision independent of provider wording.
pub(crate) fn classify_rejection(body: &str) -> Option<&'static str> {
    let text = body.to_ascii_lowercase();
    if text.contains("prompt is too long")
        || text.contains("context_length_exceeded")
        || text.contains("context window")
        || text.contains("context is too long")
        || text.contains("maximum context")
        || text.contains("too many tokens")
        || text.contains("input too long")
    {
        return Some(TOO_LONG_MESSAGE);
    }
    if text.contains("payload too large")
        || text.contains("request entity too large")
        || text.contains("request too large")
    {
        return Some(TOO_LARGE_MESSAGE);
    }
    None
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

    /// Cache markers apply to any Anthropic-compatible endpoint, including
    /// relays that forward the Messages protocol unchanged.
    fn cache_enabled(&self) -> bool {
        self.prompt_cache && self.protocol == Protocol::Anthropic
    }

    /// Extended-thinking budget derived from the configured effort level.
    ///
    /// The Messages API requires `budget_tokens` to stay below `max_tokens`.
    fn thinking_budget(&self, max_tokens: u32) -> Option<u32> {
        let share = match self.effort()? {
            "low" => 4,
            "medium" => 3,
            "high" => 2,
            "xhigh" | "max" => 1,
            _ => return None,
        };
        let budget = max_tokens / share;
        (budget >= 1_024).then_some(budget.min(max_tokens.saturating_sub(1)))
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
        self.run_inner(request, progress).await
    }

    async fn run_inner(
        &self,
        request: &Request,
        progress: Option<&mpsc::Sender<Event>>,
    ) -> Result<Vec<Event>, String> {
        let mut body = match self.protocol {
            Protocol::Anthropic => anthropic::encode(request, self.cache_enabled())?,
            Protocol::OpenaiChat => openai_chat::encode(request)?,
            Protocol::OpenaiResponses => openai_responses::encode(request)?,
        };
        apply_cache_policy(
            &mut body,
            self.protocol,
            self.cache_enabled(),
            self.cache_key.as_deref(),
        );
        if self.protocol == Protocol::Anthropic {
            if let Some(budget) = self.thinking_budget(request.max_tokens) {
                body["thinking"] = serde_json::json!({"type":"enabled","budget_tokens":budget});
            }
        }
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
        // A deadline that ended the retry loop decides the reported reason: a stalled
        // generation must not surface as an unrelated connection failure.
        let mut timed_out = false;
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
            // The deadline belongs to one attempt, not to the whole retry loop: a slow
            // generation is retried instead of consuming every attempt's budget at once.
            let response = match tokio::time::timeout(self.timeout, builder.send()).await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    // Builder errors are deterministic (for example invalid auth headers).
                    if !error.is_builder() && self.pause(attempt, 503, None, progress).await {
                        continue;
                    }
                    return Err(if timed_out {
                        TIMEOUT_MESSAGE.into()
                    } else {
                        "provider connection failed".to_owned()
                    });
                }
                Err(_) => {
                    timed_out = true;
                    if self.pause(attempt, TIMEOUT_STATUS, None, progress).await {
                        continue;
                    }
                    return Err(TIMEOUT_MESSAGE.into());
                }
            };
            let status = response.status();
            if !status.is_success() {
                let after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|value| retry_after(value, SystemTime::now()));
                let code = status.as_u16();
                // 413 is the payload limit by definition; 400 and 422 need the body to tell
                // a conversation that is too long from an ordinary invalid request.
                let classified = match code {
                    413 => Some(TOO_LARGE_MESSAGE),
                    400 | 422 => classify_rejection(&self.read_error_body(response).await),
                    _ => {
                        drop(response);
                        None
                    }
                };
                if let Some(message) = classified {
                    return Err(message.into());
                }
                if self.pause(attempt, code, after, progress).await {
                    continue;
                }
                return Err(format!("provider HTTP status {code}"));
            }
            // Existing consumers append deltas and have no reset event. Replayed
            // attempts must therefore stay silent; their validated final result
            // replaces the provisional first-attempt display at the caller.
            let live = if attempt == 0 { progress } else { None };
            let read = tokio::time::timeout(self.timeout, self.read_stream(response, live)).await;
            match read {
                Ok(Ok(events)) => return Ok(events),
                Ok(Err((message, transient))) => {
                    if transient && self.pause(attempt, 503, None, progress).await {
                        continue;
                    }
                    return Err(message);
                }
                Err(_) => {
                    timed_out = true;
                    if self.pause(attempt, TIMEOUT_STATUS, None, progress).await {
                        continue;
                    }
                    return Err(TIMEOUT_MESSAGE.into());
                }
            }
        }
        Err("retry budget exhausted".into())
    }

    /// Read a bounded error body so a rejected request can be classified without
    /// retaining the connection or leaking credentials into messages.
    async fn read_error_body(&self, response: reqwest::Response) -> String {
        const MAX_ERROR_BODY: usize = 64 * 1024;
        let mut body = String::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { break };
            if body.len().saturating_add(chunk.len()) > MAX_ERROR_BODY {
                break;
            }
            body.push_str(&String::from_utf8_lossy(&chunk));
        }
        body
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
                        // One-based retry number: the first retry is "1 из 3",
                        // not "2 из 4", and never starts at a second attempt.
                        attempt: attempt.saturating_add(1),
                        max_attempts: self.retry.retries.max(1),
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

/// Anthropic keeps cache markers on content blocks, never at the request root.
///
/// The markers are written by [`super::anthropic::encode`]; this function only
/// handles the OpenAI Responses cache key, which is a genuine top-level field.
fn apply_cache_policy(
    body: &mut serde_json::Value,
    protocol: Protocol,
    enabled: bool,
    key: Option<&str>,
) {
    if !enabled {
        return;
    }
    if protocol == Protocol::OpenaiResponses {
        if let Some(key) = key {
            body["prompt_cache_key"] = serde_json::json!(key);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod cache_policy_tests {
    use super::*;

    #[test]
    fn anthropic_cache_markers_live_on_blocks_not_the_request_root() {
        let request = Request {
            model: "claude-test".into(),
            system: "operator prompt".into(),
            messages: vec![crate::provider::canonical::Message {
                role: crate::provider::canonical::Role::User,
                content: vec![crate::provider::canonical::Content::Text {
                    text: "hello".into(),
                }],
            }],
            tools: vec![],
            max_tokens: 1024,
        };
        let cached = super::super::anthropic::encode(&request, true).unwrap();
        assert!(cached.get("cache_control").is_none());
        assert_eq!(cached["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(
            cached["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        let plain = super::super::anthropic::encode(&request, false).unwrap();
        assert_eq!(plain["system"], serde_json::json!("operator prompt"));
        assert!(plain["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
    }

    #[test]
    fn openai_responses_cache_key_is_added_and_can_be_disabled() {
        let mut openai = serde_json::json!({});
        apply_cache_policy(
            &mut openai,
            Protocol::OpenaiResponses,
            true,
            Some("random-session"),
        );
        assert_eq!(openai["prompt_cache_key"], "random-session");
        for (protocol, enabled) in [
            (Protocol::OpenaiResponses, false),
            (Protocol::OpenaiChat, true),
            (Protocol::Anthropic, true),
        ] {
            let mut body = serde_json::json!({});
            apply_cache_policy(&mut body, protocol, enabled, Some("x"));
            assert_eq!(body, serde_json::json!({}));
        }
    }
}
