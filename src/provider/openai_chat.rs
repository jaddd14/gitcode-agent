//! OpenAI Chat Completions adapter with indexed argument-fragment assembly.

use super::{
    canonical::{Content, Event, Request, Role, Usage},
    sse::Record,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Encode a canonical request as streaming Chat Completions JSON.
pub fn encode(request: &Request) -> Result<Value, String> {
    let mut messages = vec![json!({"role":"system","content":request.system})];
    for message in &request.messages {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let mut text = String::new();
        let mut parts = Vec::new();
        let mut has_attachments = false;
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for block in &message.content {
            match block {
                Content::Text {text:part} => { text.push_str(part); parts.push(json!({"type":"text","text":part})); },
                Content::ToolUse {id,name,input} => calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":input.to_string()}})),
                Content::ToolResult {tool_use_id,content,..} => results.push(json!({"role":"tool","tool_call_id":tool_use_id,"content":content})),
                Content::Thinking {..} => {},
                Content::Image {media_type,data} if message.role == Role::User => { has_attachments = true; parts.push(json!({"type":"image_url","image_url":{"url":format!("data:{media_type};base64,{data}")}})); },
                Content::Document {media_type,data} if message.role == Role::User && media_type == "application/pdf" => { has_attachments = true; parts.push(json!({"type":"file","file":{"filename":"document.pdf","file_data":format!("data:{media_type};base64,{data}")}})); },
                _ => return Err("Chat attachment type or role is not supported".into()),
            }
        }
        if !text.is_empty() || !calls.is_empty() || has_attachments {
            let content = if has_attachments {
                json!(parts)
            } else {
                json!(text)
            };
            let mut m = json!({"role":role,"content":content});
            if !calls.is_empty() {
                m["tool_calls"] = json!(calls);
            }
            messages.push(m);
        }
        messages.extend(results);
    }
    let tools: Vec<_> = request.tools.iter().map(|t|json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.input_schema}})).collect();
    let mut body = json!({"model":request.model,"messages":messages,"max_tokens":request.max_tokens,"stream":true,"stream_options":{"include_usage":true}});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    Ok(body)
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}

/// Stateful Chat stream decoder; tools are released only at DONE.
#[derive(Default)]
pub struct Stream {
    calls: BTreeMap<u64, Call>,
    reason: Option<String>,
    done: bool,
    usage: Option<Usage>,
    characters: usize,
}

impl Stream {
    /// Decode a chunk or DONE sentinel into canonical events.
    pub fn accept(&mut self, record: Record) -> Result<Vec<Event>, String> {
        if self.done {
            return Err("Chat event after DONE".into());
        }
        if record.data == "[DONE]" {
            let reason = self
                .reason
                .clone()
                .ok_or("Chat DONE without finish reason")?;
            let mut output = Vec::new();
            for call in self.calls.values() {
                if call.id.is_empty() || call.name.is_empty() {
                    return Err("incomplete Chat tool identity".into());
                }
                let input: Value = serde_json::from_str(&call.arguments)
                    .map_err(|_| "invalid Chat tool arguments")?;
                if !input.is_object() {
                    return Err("Chat tool arguments must be an object".into());
                }
                output.push(Event::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input,
                });
            }
            let usage = self.usage.clone().unwrap_or(Usage {
                output_tokens: (self.characters as u64).div_ceil(4),
                estimated: true,
                ..Usage::default()
            });
            output.push(Event::Usage { usage });
            output.push(Event::Done { reason });
            self.done = true;
            return Ok(output);
        }
        let value: Value =
            serde_json::from_str(&record.data).map_err(|_| "invalid Chat event JSON")?;
        if value.get("error").is_some() {
            return Err("Chat provider returned an error".into());
        }
        let mut output = Vec::new();
        if let Some(u) = value.get("usage").filter(|u| !u.is_null()) {
            let total_input = u["prompt_tokens"].as_u64().unwrap_or(0);
            let cached_input = u["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0)
                .min(total_input);
            let usage = Usage {
                input_tokens: total_input.saturating_sub(cached_input),
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: cached_input,
                reasoning_tokens: u["completion_tokens_details"]["reasoning_tokens"]
                    .as_u64()
                    .unwrap_or(0),
                ..Usage::default()
            };
            output.push(Event::Usage {
                usage: usage.clone(),
            });
            self.usage = Some(usage);
        }
        for choice in value["choices"].as_array().into_iter().flatten() {
            if choice["index"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.reason = Some(reason.into());
            }
            let delta = &choice["delta"];
            if let Some(text) = delta["content"].as_str() {
                self.characters += text.chars().count();
                output.push(Event::TextDelta { text: text.into() });
            }
            if let Some(text) = delta["reasoning_content"]
                .as_str()
                .or_else(|| delta["reasoning"].as_str())
            {
                output.push(Event::ThinkingDelta { text: text.into() });
            }
            for call in delta["tool_calls"].as_array().into_iter().flatten() {
                let index = call["index"].as_u64().ok_or("missing Chat tool index")?;
                let entry = self.calls.entry(index).or_default();
                if let Some(id) = call["id"].as_str() {
                    entry.id = id.into();
                }
                if let Some(name) = call["function"]["name"].as_str() {
                    entry.name.push_str(name);
                }
                if let Some(part) = call["function"]["arguments"].as_str() {
                    entry.arguments.push_str(part);
                }
                if entry.arguments.len() > 8 * 1024 * 1024 {
                    return Err("Chat tool arguments exceed limit".into());
                }
            }
        }
        Ok(output)
    }

    /// Reject partial turns before any tool can execute.
    pub fn finish(&self) -> Result<(), String> {
        if self.done {
            Ok(())
        } else {
            Err("Chat stream ended before DONE".into())
        }
    }
}
