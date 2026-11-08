//! Conservative context sizing and deterministic trimming of bulky tool outputs.

use crate::provider::canonical::{Content, Message};

/// Estimate tokens from serialized UTF-8 bytes, rounding up conservatively.
///
/// This is explicitly a heuristic rather than a tokenizer or provider usage.
pub fn estimate(messages: &[Message]) -> Result<u64, String> {
    let bytes = serde_json::to_vec(messages).map_err(|_| "cannot estimate context")?;
    Ok((bytes.len() as u64).div_ceil(3))
}

/// Trim older tool results without breaking tool-use/result pair identities.
///
/// Retains recent messages and marks every removed payload. This does not claim
/// semantic compaction: model-generated summaries require a separate request.
pub fn trim_tool_results(
    messages: &mut [Message],
    retain_recent: usize,
    max_bytes: usize,
) -> usize {
    let boundary = messages.len().saturating_sub(retain_recent);
    let mut trimmed = 0;
    for message in &mut messages[..boundary] {
        for block in &mut message.content {
            if let Content::ToolResult { content, .. } = block {
                if content.len() > max_bytes {
                    let mut end = max_bytes;
                    while !content.is_char_boundary(end) {
                        end -= 1;
                    }
                    content.truncate(end);
                    content.push_str("\n[older tool output trimmed for context]");
                    trimmed += 1;
                }
            }
        }
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::canonical::Role;
    #[test]
    fn trims_unicode_without_losing_result_id() {
        let mut messages = vec![Message {
            role: Role::User,
            content: vec![Content::ToolResult {
                tool_use_id: "call1".into(),
                content: "界".repeat(100),
                is_error: false,
            }],
        }];
        assert_eq!(trim_tool_results(&mut messages, 0, 4), 1);
        assert!(
            matches!(&messages[0].content[0], Content::ToolResult { tool_use_id, content, .. }
            if tool_use_id == "call1" && content.starts_with('界') && content.contains("trimmed"))
        );
        assert!(estimate(&messages).is_ok_and(|n| n > 0));
    }
}
