//! Live smoke test for ACP session config options (model selector) against
//! the real Claude backend. `#[ignore]`d for the same reason as `live_claude.rs`.

use wilkes_agent::session::spawn;
use wilkes_core::types::AgentBackend;

#[tokio::test]
#[ignore]
async fn reports_config_options() {
    let spawned = spawn(AgentBackend::ClaudeCode, std::env::temp_dir())
        .await
        .expect("claude-agent-acp handshake");
    let session = spawned.session;

    let options = session.config_options();
    eprintln!("config options: {options:#?}");
    session.close();

    assert!(
        !options.is_empty(),
        "expected at least one session config option"
    );
}
