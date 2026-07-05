//! Live smoke test for the read-only boundary and `fs/read_text_file` context
//! confinement (spec §6.3, §8) against the real Claude backend. `#[ignore]`d
//! for the same reason as `live_claude.rs`.

use wilkes_agent::session::{spawn, ChatEvent};
use wilkes_core::types::AgentBackend;

#[tokio::test]
#[ignore]
async fn reads_only_files_in_context() {
    let dir = tempfile::tempdir().unwrap();
    let in_context = dir.path().join("in-context.txt");
    std::fs::write(&in_context, "the-secret-marker-is-Q7X9").unwrap();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "should-never-be-read").unwrap();

    let spawned = spawn(AgentBackend::ClaudeCode, dir.path().to_path_buf())
        .await
        .expect("claude-agent-acp handshake");
    let session = spawned.session;
    let mut events = spawned.events;
    session.add_context(in_context.display().to_string(), None);

    let prompt = format!(
        "Read the file at {} using your file-reading tool and reply with exactly the value \
         after 'the-secret-marker-is-'. Do not guess -- only answer with what the tool returns.",
        in_context.display()
    );

    let mut send = Box::pin(session.send("t1".into(), prompt));
    let mut text = String::new();
    let mut saw_read_tool = false;
    let stop_reason = loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(ChatEvent::TextDelta { delta, .. }) => text.push_str(&delta),
                Some(ChatEvent::ThoughtDelta { .. }) => {}
                Some(ChatEvent::ToolCall { .. }) => saw_read_tool = true,
                Some(ChatEvent::PermissionRequest { .. }) => {}
                Some(ChatEvent::SessionError { message }) => panic!("session error: {message}"),
                Some(ChatEvent::ConfigOptionsUpdated { .. }) => {}
                None => panic!("event stream closed before the turn finished"),
            },
            result = &mut send => break result.expect("prompt turn"),
        }
    };

    assert_eq!(stop_reason, "end_turn");
    assert!(saw_read_tool, "expected the agent to use a read tool");
    assert!(text.contains("Q7X9"), "got: {text:?}");
}
