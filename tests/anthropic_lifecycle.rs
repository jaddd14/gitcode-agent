//! Reject malformed message lifecycles before any tool is executable.
#![allow(clippy::unwrap_used)]

use gitcode::provider::{anthropic::Stream, sse::Record};
use serde_json::{json, Value};

fn accept(stream: &mut Stream, value: Value) -> Result<(), String> {
    stream
        .accept(Record {
            event: String::new(),
            data: value.to_string(),
        })
        .map(|_| ())
}

#[test]
fn rejects_missing_start_duplicate_blocks_and_non_object_arguments() {
    assert!(accept(&mut Stream::default(), json!({"type":"message_stop"})).is_err());
    let mut stream = Stream::default();
    accept(&mut stream, json!({"type":"message_start","message":{}})).unwrap();
    assert!(accept(&mut stream, json!({"type":"message_start","message":{}})).is_err());

    let mut stream = Stream::default();
    accept(&mut stream, json!({"type":"message_start","message":{}})).unwrap();
    let block = json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"Read","input":{}}});
    accept(&mut stream, block.clone()).unwrap();
    assert!(accept(&mut stream, block).is_err());
    accept(&mut stream, json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"[]"}})).unwrap();
    assert!(accept(&mut stream, json!({"type":"content_block_stop","index":0})).is_err());
    assert!(stream.finish().is_err());
}

#[test]
fn signed_thinking_replays_verbatim() {
    use gitcode::provider::canonical::{Content, Event, Message, Request, Role};
    let mut stream = Stream::default();
    accept(&mut stream, json!({"type":"message_start","message":{}})).unwrap();
    accept(&mut stream, json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}})).unwrap();
    accept(&mut stream, json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"original reasoning"}})).unwrap();
    for signature in ["opaque-", "signature"] {
        accept(&mut stream, json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":signature}})).unwrap();
    }
    let events = stream
        .accept(Record {
            event: String::new(),
            data: json!({"type":"content_block_stop","index":0}).to_string(),
        })
        .unwrap();
    let Event::ThinkingComplete {
        thinking,
        signature,
    } = events.into_iter().next().unwrap()
    else {
        panic!("missing replay block")
    };
    let request = Request {
        model: "mock".into(),
        system: String::new(),
        tools: vec![],
        max_tokens: 32,
        messages: vec![Message {
            role: Role::Assistant,
            content: vec![Content::Thinking {
                thinking,
                signature,
                encrypted_content: None,
            }],
        }],
    };
    let encoded = gitcode::provider::anthropic::encode(&request).unwrap();
    assert_eq!(
        encoded["messages"][0]["content"][0],
        json!({"type":"thinking","thinking":"original reasoning","signature":"opaque-signature"})
    );
}

#[test]
fn rejects_unopened_deltas_wrong_types_and_premature_end() {
    let mut stream = Stream::default();
    accept(&mut stream, json!({"type":"message_start","message":{}})).unwrap();
    assert!(accept(
        &mut stream,
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}})
    )
    .is_err());
    accept(
        &mut stream,
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
    )
    .unwrap();
    assert!(accept(&mut stream, json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"x"}})).is_err());
    assert!(accept(&mut stream, json!({"type":"message_stop"})).is_err());
    accept(&mut stream, json!({"type":"content_block_stop","index":0})).unwrap();
    accept(
        &mut stream,
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
    )
    .unwrap();
    accept(&mut stream, json!({"type":"message_stop"})).unwrap();
    stream.finish().unwrap();
    assert!(accept(&mut stream, json!({"type":"message_stop"})).is_err());
}
