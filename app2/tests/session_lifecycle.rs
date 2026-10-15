use agent_client_protocol::AcpAgent;
use agent_client_protocol::schema::v1::{McpServerStdio, SessionId};
use kid_agentic_coding_app2::{Client, McpServer};
use std::path::PathBuf;
use tokio::sync::mpsc;

fn agent_args(extra: &[&str]) -> Vec<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
    let mut args = vec![
        "cargo".to_owned(),
        "run".to_owned(),
        "--manifest-path".to_owned(),
        manifest.to_string_lossy().into_owned(),
        "-p".to_owned(),
        "lorem-agent".to_owned(),
        "--".to_owned(),
    ];
    args.extend(extra.iter().map(|argument| (*argument).to_owned()));
    args
}

async fn connected(extra: &[&str]) -> Client<kid_agentic_coding_app2::ClientConnected> {
    let agent = AcpAgent::from_args(agent_args(extra)).unwrap();
    let (events, _receiver) = mpsc::channel(32);
    Client::new(agent).connect(events).await.unwrap()
}
#[tokio::test]
async fn sessions_carry_their_own_ids_and_mcp_servers() {
    let client = connected(&[]).await;
    let server = McpServer::Stdio(McpServerStdio::new("integration-test-server", "/bin/true"));
    let session = client.new_session(vec![server.clone()]).await.unwrap();

    assert_ne!(session.id(), &SessionId::new(""));
    assert_eq!(session.mcp_servers(), &[server]);
    let _ = client.disconnect().await.unwrap();
}

#[tokio::test]
async fn sessions_can_be_closed_and_deleted_when_supported() {
    let client = connected(&["--session-delete"]).await;
    let session = client.new_session(Vec::new()).await.unwrap();
    let deleted = session.close().await.unwrap().delete().await.unwrap();

    drop(deleted);
    let _ = client.disconnect().await.unwrap();
}

#[tokio::test]
async fn delete_without_capability_fails_before_sending_a_request() {
    let client = connected(&[]).await;
    let session = client.new_session(Vec::new()).await.unwrap();
    let closed = session.close().await.unwrap();

    assert!(closed.delete().await.is_err());
    let _ = client.disconnect().await.unwrap();
}

#[tokio::test]
async fn one_connection_can_own_multiple_sessions() {
    let client = connected(&[]).await;
    let first = client.new_session(Vec::new()).await.unwrap();
    let second = client.new_session(Vec::new()).await.unwrap();

    assert_ne!(first.id(), second.id());
    let _ = client.disconnect().await.unwrap();
}
