//! End-to-end smoke test against the real `@agentclientprotocol/claude-agent-acp`
//! subprocess. Requires the adapter package + a working Claude Code login, so it
//! is `#[ignore]`d by default -- run explicitly with
//! `cargo test -p wilkes-agent --test live_claude -- --ignored`.

use wilkes_agent::session::{spawn, ChatEvent};
use wilkes_core::types::AgentBackend;

#[tokio::test]
#[ignore]
async fn claude_backend_completes_a_turn() {
    let spawned = spawn(AgentBackend::ClaudeCode, std::env::temp_dir())
        .await
        .expect("claude-agent-acp handshake");
    let session = spawned.session;
    let mut events = spawned.events;

    let turn_id = "live-test-turn".to_string();
    let mut send = Box::pin(session.send(
        turn_id,
        "Reply with exactly the word PONG and nothing else.".into(),
    ));

    let mut text = String::new();
    let stop_reason = loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(ChatEvent::TextDelta { delta, .. }) => text.push_str(&delta),
                Some(ChatEvent::ThoughtDelta { .. }) => {}
                Some(ChatEvent::ToolCall { .. }) => {}
                Some(ChatEvent::PermissionRequest { .. }) => {}
                Some(ChatEvent::SessionError { message }) => panic!("session error: {message}"),
                Some(ChatEvent::ConfigOptionsUpdated { .. }) => {}
                None => panic!("event stream closed before the turn finished"),
            },
            result = &mut send => break result.expect("prompt turn"),
        }
    };

    assert_eq!(stop_reason, "end_turn");
    assert!(text.to_uppercase().contains("PONG"), "got: {text:?}");
}
