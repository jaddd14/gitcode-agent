//! Canonical conversation types preserve structured content across adapters.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Conversation speaker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Operator instruction.
    System,
    /// User input or tool response.
    User,
    /// Model output.
    Assistant,
}

/// Structured message content, including opaque provider reasoning state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    /// Visible natural-language output.
    Text {
        /// UTF-8 content.
        text: String,
    },
    /// Reasoning text with optional opaque replay metadata.
    Thinking {
        /// Provider-visible reasoning summary.
        thinking: String,
        /// Anthropic replay signature.
        signature: Option<String>,
        /// Responses encrypted reasoning state.
        encrypted_content: Option<String>,
    },
    /// Requested tool operation.
    ToolUse {
        /// Unique call identifier.
        id: String,
        /// Registered tool name.
        name: String,
        /// Parsed tool arguments.
        input: Value,
    },
    /// Result paired with exactly one tool operation.
    ToolResult {
        /// Original tool call identifier.
        tool_use_id: String,
        /// Textual result.
        content: String,
        /// Whether execution failed.
        is_error: bool,
    },
    /// Inline image attachment.
    Image {
        /// Validated image media type.
        media_type: String,
        /// Base64 content without newlines.
        data: String,
    },
    /// Inline document attachment.
    Document {
        /// Document media type.
        media_type: String,
        /// Base64 content.
        data: String,
    },
}

/// One append-only conversation message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// Speaker role.
    pub role: Role,
    /// Ordered structured blocks.
    pub content: Vec<Content>,
}

impl Message {
    /// Construct a plain-text message.
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![Content::Text { text: text.into() }],
        }
    }
}

/// Per-request accounting; absent provider values remain explicitly estimated.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    /// Non-cached input tokens.
    pub input_tokens: u64,
    /// Generated tokens.
    pub output_tokens: u64,
    /// Cache reads.
    pub cache_read_tokens: u64,
    /// Cache writes.
    pub cache_write_tokens: u64,
    /// Reasoning subset, when reported.
    pub reasoning_tokens: u64,
    /// Whether a heuristic supplied any accounting.
    pub estimated: bool,
}

/// Registered tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Tool name.
    pub name: String,
    /// Model-visible behavior contract.
    pub description: String,
    /// JSON Schema for arguments.
    pub input_schema: Value,
}

/// A protocol-independent model request.
#[derive(Debug, Clone)]
pub struct Request {
    /// Selected model identifier.
    pub model: String,
    /// Stable operator prompt.
    pub system: String,
    /// Complete retained conversation.
    pub messages: Vec<Message>,
    /// Available tools.
    pub tools: Vec<Tool>,
    /// Maximum generated tokens.
    pub max_tokens: u32,
}

/// Canonical incremental output for renderers and accounting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Visible text fragment.
    TextDelta {
        /// Fragment content.
        text: String,
    },
    /// Reasoning fragment.
    ThinkingDelta {
        /// Fragment content.
        text: String,
    },
    /// Completed reasoning block retained verbatim for provider replay.
    ThinkingComplete {
        /// Original reasoning text.
        thinking: String,
        /// Opaque provider signature.
        signature: Option<String>,
    },
    /// Complete validated tool call.
    ToolCall {
        /// Unique call identifier.
        id: String,
        /// Registered name.
        name: String,
        /// Parsed arguments.
        input: Value,
    },
    /// Updated request usage.
    Usage {
        /// Provider usage counters.
        usage: Usage,
    },
    /// Explicit successful end of a provider message.
    Done {
        /// Provider stop reason.
        reason: String,
    },
}
