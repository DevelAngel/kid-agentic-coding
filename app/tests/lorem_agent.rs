//! Integration test: runs `PromptRunner` against the real `lorem-agent`
//! binary over a subprocess ACP connection, so the wiring is exercised
//! end-to-end without depending on a real LLM.

use kid_agentic_coding::PromptRunner;

#[tokio::test]
async fn prompt_run_returns_lorem_ipsum_text() {
    let agent = PromptRunner::parse_agent_args(&[
        "cargo".to_owned(),
        "run".to_owned(),
        "--quiet".to_owned(),
        "-p".to_owned(),
        "lorem-agent".to_owned(),
    ])
    .expect("valid agent command");

    let response = PromptRunner::run(agent, "Hello, agent!")
        .await
        .expect("prompt run succeeds");

    assert!(!response.trim().is_empty());
}
