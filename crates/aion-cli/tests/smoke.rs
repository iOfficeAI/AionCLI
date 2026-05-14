use aion_cli::client::{parse_ws_message, ServerEvent};

#[test]
fn parse_text_event() {
    let raw = r#"{"name":"message.stream","data":{"conversation_id":"c1","msg_id":"m1","type":"text","data":{"content":"Hello "},"hidden":false}}"#;
    let event = parse_ws_message(raw);
    assert_eq!(event, Some(ServerEvent::StreamText { content: "Hello ".to_string() }));
}

#[test]
fn parse_finish_event() {
    let raw = r#"{"name":"message.stream","data":{"conversation_id":"c1","msg_id":"m1","type":"finish","data":{},"hidden":false}}"#;
    let event = parse_ws_message(raw);
    assert_eq!(event, Some(ServerEvent::StreamFinish));
}

#[test]
fn parse_error_event() {
    let raw = r#"{"name":"message.stream","data":{"conversation_id":"c1","msg_id":"m1","type":"error","data":{"message":"Rate limit"},"hidden":false}}"#;
    let event = parse_ws_message(raw);
    assert_eq!(event, Some(ServerEvent::StreamError { message: "Rate limit".to_string() }));
}

#[test]
fn parse_unknown_event_returns_none() {
    let raw = r#"{"name":"some.other.event","data":{}}"#;
    let event = parse_ws_message(raw);
    assert_eq!(event, None);
}

#[test]
fn parse_thinking_event() {
    let raw = r#"{"name":"message.stream","data":{"conversation_id":"c1","msg_id":"m1","type":"thinking","data":{"content":"hmm..."},"hidden":false}}"#;
    let event = parse_ws_message(raw);
    assert_eq!(event, Some(ServerEvent::StreamThinking { content: "hmm...".to_string() }));
}

#[test]
fn parse_invalid_json_returns_none() {
    let event = parse_ws_message("not json");
    assert_eq!(event, None);
}
