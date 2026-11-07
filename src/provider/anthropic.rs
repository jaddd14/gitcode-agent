//! Anthropic Messages encoding and indexed streaming block assembly.

use super::{
    canonical::{Content, Event, Request, Role, Usage},
    sse::Record,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Encode the authoritative Messages wire format.
pub fn encode(request: &Request) -> Result<Value, String> {
    let mut messages = Vec::new();
    for message in &request.messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => return Err("system messages belong in the operator prompt".into()),
        };
        let mut content = Vec::new();
        for block in &message.content {
            content.push(match block {
                Content::Text {text} => json!({"type":"text","text":text}),
                Content::Thinking {thinking,signature,..} => json!({"type":"thinking","thinking":thinking,"signature":signature}),
                Content::ToolUse {id,name,input} => json!({"type":"tool_use","id":id,"name":name,"input":input}),
                Content::ToolResult {tool_use_id,content,is_error} => json!({"type":"tool_result","tool_use_id":tool_use_id,"content":content,"is_error":is_error}),
                Content::Image {media_type,data} => json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}}),
                Content::Document {media_type,data} => json!({"type":"document","source":{"type":"base64","media_type":media_type,"data":data}}),
            });
        }
        messages.push(json!({"role":role,"content":content}));
    }
    let tools: Vec<_> = request
        .tools
        .iter()
        .map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.input_schema}))
        .collect();
    Ok(
        json!({"model":request.model,"system":request.system,"messages":messages,"tools":tools,"max_tokens":request.max_tokens,"stream":true}),
    )
}

#[derive(Default)]
struct Block {
    id: String,
    name: String,
    input: String,
    initial: Option<Value>,
}

/// Decoder preserves argument fragments until a block closes successfully.
#[derive(Default)]
pub struct Stream {
    blocks: BTreeMap<u64, Block>,
    usage: Usage,
    reason: String,
    done: bool,
}

impl Stream {
    /// Translate one named SSE record into canonical incremental events.
    pub fn accept(&mut self, record: Record) -> Result<Vec<Event>, String> {
        if record.event == "ping" {
            return Ok(vec![]);
        }
        let value: Value =
            serde_json::from_str(&record.data).map_err(|_| "invalid provider event JSON")?;
        let kind = value["type"].as_str().unwrap_or(&record.event);
        let mut output = Vec::new();
        match kind {
            "message_start" => self.update_usage(&value["message"]["usage"]),
            "content_block_start" => {
                let index = value["index"].as_u64().ok_or("missing block index")?;
                let block = &value["content_block"];
                if block["type"] == "tool_use" {
                    self.blocks.insert(
                        index,
                        Block {
                            id: block["id"].as_str().ok_or("missing tool id")?.into(),
                            name: block["name"].as_str().ok_or("missing tool name")?.into(),
                            input: String::new(),
                            initial: Some(block["input"].clone()),
                        },
                    );
                }
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => output.push(Event::TextDelta {
                        text: delta["text"].as_str().ok_or("missing text delta")?.into(),
                    }),
                    Some("thinking_delta") => output.push(Event::ThinkingDelta {
                        text: delta["thinking"]
                            .as_str()
                            .ok_or("missing thinking delta")?
                            .into(),
                    }),
                    Some("input_json_delta") => {
                        let index = value["index"].as_u64().ok_or("missing block index")?;
                        let block = self
                            .blocks
                            .get_mut(&index)
                            .ok_or("arguments for unknown tool block")?;
                        block.input.push_str(
                            delta["partial_json"]
                                .as_str()
                                .ok_or("missing argument fragment")?,
                        );
                        if block.input.len() > 8 * 1024 * 1024 {
                            return Err("tool input exceeds limit".into());
                        }
                    }
                    Some("signature_delta") => {}
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = value["index"].as_u64().ok_or("missing block index")?;
                if let Some(block) = self.blocks.remove(&index) {
                    let input = if block.input.is_empty() {
                        block.initial.unwrap_or(json!({}))
                    } else {
                        serde_json::from_str(&block.input)
                            .map_err(|_| "invalid tool argument JSON")?
                    };
                    output.push(Event::ToolCall {
                        id: block.id,
                        name: block.name,
                        input,
                    });
                }
            }
            "message_delta" => {
                self.update_usage(&value["usage"]);
                self.reason = value["delta"]["stop_reason"]
                    .as_str()
                    .unwrap_or("unknown")
                    .into();
                output.push(Event::Usage {
                    usage: self.usage.clone(),
                });
            }
            "message_stop" => {
                if !self.blocks.is_empty() {
                    return Err("message stopped with incomplete tool arguments".into());
                }
                self.done = true;
                output.push(Event::Done {
                    reason: self.reason.clone(),
                });
            }
            "error" => return Err("provider returned a streaming error".into()),
            _ => {}
        }
        Ok(output)
    }

    /// Require an explicit message_stop before a turn can execute tools.
    pub fn finish(&self) -> Result<(), String> {
        if self.done && self.blocks.is_empty() {
            Ok(())
        } else {
            Err("provider stream ended before message_stop".into())
        }
    }

    fn update_usage(&mut self, value: &Value) {
        for (field, target) in [
            ("input_tokens", &mut self.usage.input_tokens),
            ("output_tokens", &mut self.usage.output_tokens),
            ("cache_read_input_tokens", &mut self.usage.cache_read_tokens),
            (
                "cache_creation_input_tokens",
                &mut self.usage.cache_write_tokens,
            ),
        ] {
            if let Some(n) = value[field].as_u64() {
                *target = n;
            }
        }
    }
}
