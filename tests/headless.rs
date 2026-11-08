//! Tests for Milestone M13 headless stream-json events, exit codes, completions, and doctor.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gitcode::{headless, provider};

use headless::{
    generate_completions, parse_stream_json_input, run_doctor, ExitCode, HeadlessEvent,
    InputFormat, OutputFormat,
};
use provider::canonical::Usage;
use serde_json::json;

#[test]
fn stream_json_events_completions_and_doctor_work() {
    assert_eq!(
        OutputFormat::parse("stream-json").unwrap(),
        OutputFormat::StreamJson
    );
    assert_eq!(
        InputFormat::parse("stream-json").unwrap(),
        InputFormat::StreamJson
    );
    assert_eq!(ExitCode::Success as i32, 0);
    assert_eq!(ExitCode::PermissionDenied as i32, 3);

    let init = HeadlessEvent::Init {
        session_id: "s-1".into(),
        model: "claude-opus-5-5".into(),
        provider: "anthropic".into(),
        tools: vec!["Read".into(), "Bash".into()],
    };
    let line = init.to_jsonl().unwrap();
    assert!(line.contains("\"type\":\"init\""));
    let back: HeadlessEvent = serde_json::from_str(&line).unwrap();
    assert_eq!(back, init);

    let res_ev = HeadlessEvent::Result {
        text: "Готово.".into(),
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        requests: 1,
        duration_ms: 42,
        is_error: false,
    };
    assert!(res_ev.to_jsonl().unwrap().contains("\"type\":\"result\""));

    let prompts =
        parse_stream_json_input("{\"prompt\":\"first\"}\n{\"message\":{\"content\":\"second\"}}\n")
            .unwrap();
    assert_eq!(prompts, vec!["first", "second"]);

    for sh in ["bash", "zsh", "fish"] {
        let script = generate_completions(sh).unwrap();
        assert!(script.contains("gitcode"));
    }
    assert!(generate_completions("csh").is_err());

    let doc = run_doctor(std::env::temp_dir().as_path(), true, true);
    assert!(doc.healthy);
    let doc_json = serde_json::to_value(&doc).unwrap();
    assert_eq!(doc_json["healthy"], json!(true));
}
