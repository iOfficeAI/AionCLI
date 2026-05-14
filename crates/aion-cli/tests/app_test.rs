use aion_cli::app::{App, AppState, ChatMessage, MessageRole};
use aion_cli::client::ServerEvent;

#[test]
fn initial_state_is_connecting() {
    let app = App::new("acp".to_string(), None);
    assert_eq!(app.state, AppState::Connecting);
    assert!(app.messages.is_empty());
    assert!(app.input.is_empty());
}

#[test]
fn connected_event_transitions_to_idle() {
    let mut app = App::new("acp".to_string(), None);
    app.handle_server_event(ServerEvent::Connected);
    assert_eq!(app.state, AppState::Idle);
}

#[test]
fn submit_input_pushes_user_message() {
    let mut app = App::new("acp".to_string(), None);
    app.state = AppState::Idle;
    app.input = "hello".to_string();
    app.cursor_pos = 5;

    let text = app.submit_input();
    assert_eq!(text, Some("hello".to_string()));
    assert_eq!(app.state, AppState::Sending);
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].role, MessageRole::User);
    assert_eq!(app.messages[0].content, "hello");
    assert!(app.input.is_empty());
}

#[test]
fn submit_empty_input_returns_none() {
    let mut app = App::new("acp".to_string(), None);
    app.state = AppState::Idle;
    app.input = "   ".to_string();

    let text = app.submit_input();
    assert_eq!(text, None);
    assert_eq!(app.state, AppState::Idle);
}

#[test]
fn stream_text_appends_to_assistant_message() {
    let mut app = App::new("acp".to_string(), None);
    app.state = AppState::Sending;

    app.handle_server_event(ServerEvent::StreamText { content: "Hello ".to_string() });
    assert_eq!(app.state, AppState::Streaming);
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].content, "Hello ");

    app.handle_server_event(ServerEvent::StreamText { content: "world".to_string() });
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].content, "Hello world");
}

#[test]
fn stream_finish_transitions_to_idle() {
    let mut app = App::new("acp".to_string(), None);
    app.state = AppState::Streaming;
    app.messages.push(ChatMessage {
        role: MessageRole::Assistant,
        content: "done".to_string(),
    });

    app.handle_server_event(ServerEvent::StreamFinish);
    assert_eq!(app.state, AppState::Idle);
}

#[test]
fn stream_error_adds_system_message() {
    let mut app = App::new("acp".to_string(), None);
    app.state = AppState::Streaming;

    app.handle_server_event(ServerEvent::StreamError { message: "fail".to_string() });
    assert_eq!(app.state, AppState::Idle);
    assert_eq!(app.messages.last().unwrap().role, MessageRole::System);
    assert!(app.messages.last().unwrap().content.contains("fail"));
}

#[test]
fn disconnected_sets_should_quit() {
    let mut app = App::new("acp".to_string(), None);
    app.handle_server_event(ServerEvent::Disconnected);
    assert!(app.should_quit);
}

#[test]
fn insert_and_delete_char() {
    let mut app = App::new("acp".to_string(), None);
    app.insert_char('h');
    app.insert_char('i');
    assert_eq!(app.input, "hi");
    assert_eq!(app.cursor_pos, 2);

    app.delete_char();
    assert_eq!(app.input, "h");
    assert_eq!(app.cursor_pos, 1);
}

#[test]
fn cursor_movement() {
    let mut app = App::new("acp".to_string(), None);
    app.input = "abc".to_string();
    app.cursor_pos = 3;

    app.move_cursor_left();
    assert_eq!(app.cursor_pos, 2);

    app.move_cursor_left();
    assert_eq!(app.cursor_pos, 1);

    app.move_cursor_right();
    assert_eq!(app.cursor_pos, 2);
}
