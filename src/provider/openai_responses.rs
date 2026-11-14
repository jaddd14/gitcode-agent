//! Stateless Responses adapter preserving opaque reasoning items on input.

use super::{
    canonical::{Content, Event, Request, Role, Usage},
    sse::Record,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Encode a stateless Responses request with encrypted reasoning enabled.
pub fn encode(request: &Request) -> Result<Value, String> {
    let mut input = Vec::new();
    for message in &request.messages {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        for block in &message.content {
            match block {
                Content::Text {text} => input.push(json!({"role":role,"content":[{"type":if message.role==Role::Assistant {"output_text"} else {"input_text"},"text":text}]})),
                Content::ToolUse {id,name,input:arguments} => input.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments.to_string()})),
                Content::ToolResult {tool_use_id,content,..} => input.push(json!({"type":"function_call_output","call_id":tool_use_id,"output":content})),
                Content::Thinking {encrypted_content:Some(data),..} => input.push(json!({"type":"reasoning","encrypted_content":data,"summary":[]})),
                Content::Thinking {..} => {},
                Content::Image {media_type,data} if message.role == Role::User => input.push(json!({"role":"user","content":[{"type":"input_image","image_url":format!("data:{media_type};base64,{data}")}]})),
                Content::Document {media_type,data} if message.role == Role::User && media_type == "application/pdf" => input.push(json!({"role":"user","content":[{"type":"input_file","filename":"document.pdf","file_data":format!("data:{media_type};base64,{data}")}]})),
                _=>return Err("Responses attachment type or role is not supported".into()),
            }
        }
    }
    let tools:Vec<_>=request.tools.iter().map(|t|json!({"type":"function","name":t.name,"description":t.description,"parameters":t.input_schema})).collect();
    Ok(
        json!({"model":request.model,"instructions":request.system,"input":input,"tools":tools,"max_output_tokens":request.max_tokens,"stream":true,"store":false,"include":["reasoning.encrypted_content"]}),
    )
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
    closed: bool,
}

/// Responses event assembler; function calls remain buffered until completed.
#[derive(Default)]
pub struct Stream {
    calls: BTreeMap<String, Call>,
    done: bool,
}

impl Stream {
    /// Decode one Responses SSE event, rejecting failed and incomplete turns.
    pub fn accept(&mut self, record: Record) -> Result<Vec<Event>, String> {
        if self.done {
            return Err("Responses event after completion".into());
        }
        let value: Value =
            serde_json::from_str(&record.data).map_err(|_| "invalid Responses event JSON")?;
        let kind = value["type"].as_str().unwrap_or(&record.event);
        let mut output = Vec::new();
        match kind {
            "response.output_text.delta" => output.push(Event::TextDelta {
                text: value["delta"]
                    .as_str()
                    .ok_or("missing Responses text delta")?
                    .into(),
            }),
            "response.reasoning_summary_text.delta" => output.push(Event::ThinkingDelta {
                text: value["delta"]
                    .as_str()
                    .ok_or("missing Responses reasoning delta")?
                    .into(),
            }),
            "response.output_item.added" => {
                let item = &value["item"];
                if item["type"] == "function_call" {
                    let key = item["id"].as_str().ok_or("missing Responses item id")?;
                    if self.calls.contains_key(key) {
                        return Err("duplicate Responses function item".into());
                    }
                    self.calls.insert(
                        key.into(),
                        Call {
                            id: item["call_id"]
                                .as_str()
                                .ok_or("missing Responses call id")?
                                .into(),
                            name: item["name"]
                                .as_str()
                                .ok_or("missing Responses function name")?
                                .into(),
                            ..Call::default()
                        },
                    );
                }
            }
            "response.function_call_arguments.delta" => {
                let key = value["item_id"]
                    .as_str()
                    .ok_or("missing Responses item reference")?;
                let call = self
                    .calls
                    .get_mut(key)
                    .ok_or("unknown Responses function item")?;
                call.arguments.push_str(
                    value["delta"]
                        .as_str()
                        .ok_or("missing Responses arguments delta")?,
                );
                if call.arguments.len() > 8 * 1024 * 1024 {
                    return Err("Responses arguments exceed limit".into());
                }
            }
            "response.output_item.done" => {
                let item = &value["item"];
                if item["type"] == "function_call" {
                    let key = item["id"].as_str().ok_or("missing Responses item id")?;
                    let call = self
                        .calls
                        .get_mut(key)
                        .ok_or("unknown completed Responses item")?;
                    if let Some(arguments) = item["arguments"].as_str() {
                        call.arguments = arguments.into();
                    }
                    call.closed = true;
                }
            }
            "response.completed" => {
                for call in self.calls.values() {
                    if !call.closed {
                        return Err("Responses completed with unfinished function".into());
                    }
                    let input: Value = serde_json::from_str(&call.arguments)
                        .map_err(|_| "invalid Responses function arguments")?;
                    if !input.is_object() {
                        return Err("Responses function arguments must be an object".into());
                    }
                    output.push(Event::ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        input,
                    });
                }
                let u = &value["response"]["usage"];
                let total_input = u["input_tokens"].as_u64().unwrap_or(0);
                let cached_input = u["input_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0)
                    .min(total_input);
                output.push(Event::Usage {
                    usage: Usage {
                        input_tokens: total_input.saturating_sub(cached_input),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        cache_read_tokens: cached_input,
                        reasoning_tokens: u["output_tokens_details"]["reasoning_tokens"]
                            .as_u64()
                            .unwrap_or(0),
                        estimated: u.is_null(),
                        ..Usage::default()
                    },
                });
                output.push(Event::Done {
                    reason: if self.calls.is_empty() {
                        "end_turn"
                    } else {
                        "tool_use"
                    }
                    .into(),
                });
                self.done = true;
            }
            "response.incomplete"
                if value["response"]["incomplete_details"]["reason"] == "max_output_tokens" =>
            {
                return Err("Responses output token limit reached".into());
            }
            "response.incomplete"
                if value["response"]["incomplete_details"]["reason"] == "content_filter" =>
            {
                return Err("Responses content filtered".into());
            }
            "response.failed" | "response.incomplete" | "error" => {
                return Err("Responses request failed or was incomplete".into())
            }
            _ => {}
        }
        Ok(output)
    }

    /// Require response.completed before returning executable calls.
    pub fn finish(&self) -> Result<(), String> {
        if self.done {
            Ok(())
        } else {
            Err("Responses stream ended before completion".into())
        }
    }
}
