//! Bundled endpoint presets contain no keys and never contact the network.

use crate::config::Protocol;

/// Public provider preset selected by a stable identifier.
#[derive(Debug, Clone)]
pub struct Preset {
    /// Stable preset name.
    pub name: &'static str,
    /// API root, including v1 when required by OpenAI formats.
    pub base_url: &'static str,
    /// Wire protocol.
    pub protocol: Protocol,
    /// Environment variable holding credentials; empty for local providers.
    pub key_env: &'static str,
}

/// Return built-in connection defaults without inventing model capabilities.
pub fn presets() -> Vec<Preset> {
    vec![
        Preset {
            name: "anthropic",
            base_url: "https://api.anthropic.com",
            protocol: Protocol::Anthropic,
            key_env: "ANTHROPIC_API_KEY",
        },
        Preset {
            name: "openai",
            base_url: "https://api.openai.com/v1",
            protocol: Protocol::OpenaiResponses,
            key_env: "OPENAI_API_KEY",
        },
        Preset {
            name: "openrouter",
            base_url: "https://openrouter.ai/api/v1",
            protocol: Protocol::OpenaiChat,
            key_env: "OPENROUTER_API_KEY",
        },
        Preset {
            name: "dashscope-chat",
            base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            protocol: Protocol::OpenaiChat,
            key_env: "DASHSCOPE_API_KEY",
        },
        Preset {
            name: "dashscope-anthropic",
            base_url: "https://dashscope-intl.aliyuncs.com/apps/anthropic",
            protocol: Protocol::Anthropic,
            key_env: "DASHSCOPE_API_KEY",
        },
        Preset {
            name: "ollama",
            base_url: "http://localhost:11434/v1",
            protocol: Protocol::OpenaiChat,
            key_env: "",
        },
        Preset {
            name: "lm-studio",
            base_url: "http://localhost:1234/v1",
            protocol: Protocol::OpenaiChat,
            key_env: "",
        },
        Preset {
            name: "generic-openai",
            base_url: "http://localhost:8000/v1",
            protocol: Protocol::OpenaiChat,
            key_env: "GITCODE_API_KEY",
        },
        Preset {
            name: "generic-anthropic",
            base_url: "http://localhost:8000",
            protocol: Protocol::Anthropic,
            key_env: "GITCODE_API_KEY",
        },
    ]
}

/// Provider-specific request compatibility switches.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Quirks {
    /// Prefer max_completion_tokens over max_tokens in Chat requests.
    pub max_completion_tokens: bool,
    /// Omit stream_options for providers that reject usage streaming.
    pub omit_stream_options: bool,
    /// Omit parallel_tool_calls for older compatible endpoints.
    pub omit_parallel_tool_calls: bool,
}

/// Apply narrowly scoped compatibility adjustments to a Chat request.
pub fn apply_chat_quirks(body: &mut serde_json::Value, quirks: &Quirks) {
    if let Some(map) = body.as_object_mut() {
        if quirks.max_completion_tokens {
            if let Some(limit) = map.remove("max_tokens") {
                map.insert("max_completion_tokens".into(), limit);
            }
        }
        if quirks.omit_stream_options {
            map.remove("stream_options");
        }
        if quirks.omit_parallel_tool_calls {
            map.remove("parallel_tool_calls");
        }
    }
}
