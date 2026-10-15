use std::fmt;
use std::path::PathBuf;

use agent_client_protocol::AcpAgent;
use agent_client_protocol::schema::v1::{McpServerStdio, SessionId, SessionNotification};
use cucumber::{World as _, given, then, when};
use kid_agentic_coding_app2::{
    Client, ClientConnected, McpServer, Session, SessionActive, SessionClosed, SessionDeleted,
};
use tokio::sync::mpsc;

#[derive(Default, cucumber::World)]
struct World {
    client: Option<Client<ClientConnected>>,
    events: Option<mpsc::Receiver<SessionNotification>>,
    session: Option<Session<SessionActive>>,
    closed_session: Option<Session<SessionClosed>>,
    session_id: Option<SessionId>,
    second_session_id: Option<SessionId>,
    notifications: Vec<SessionNotification>,
    deleted: bool,
    delete_rejected: bool,
}

impl fmt::Debug for World {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("World")
            .field("connected", &self.client.is_some())
            .field("has_session", &self.session.is_some())
            .field("has_closed_session", &self.closed_session.is_some())
            .field("deleted", &self.deleted)
            .field("delete_rejected", &self.delete_rejected)
            .finish()
    }
}

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

async fn connect(world: &mut World, session_delete: bool) {
    let extra = if session_delete {
        vec!["--session-delete"]
    } else {
        Vec::new()
    };
    let agent = AcpAgent::from_args(agent_args(&extra)).unwrap();
    let (events, receiver) = mpsc::channel(32);
    world.client = Some(Client::new(agent).connect(events).await.unwrap());
    world.events = Some(receiver);
}

#[given("a lorem-agent connection with session deletion support")]
async fn connected_with_session_delete(world: &mut World) {
    connect(world, true).await;
}

#[given("a lorem-agent connection without session deletion support")]
async fn connected_without_session_delete(world: &mut World) {
    connect(world, false).await;
}

#[when("I create a session")]
async fn create_session(world: &mut World) {
    let session = world
        .client
        .as_ref()
        .expect("the agent connection is available")
        .new_session(Vec::new())
        .await
        .unwrap();
    world.session_id = Some(session.id().clone());
    world.session = Some(session);
}

#[when(regex = r#"^I create a session with MCP server "([^"]+)"$"#)]
async fn create_session_with_mcp_server(world: &mut World, name: String) {
    let server = McpServer::Stdio(McpServerStdio::new(name, "/bin/true"));
    let session = world
        .client
        .as_ref()
        .expect("the agent connection is available")
        .new_session(vec![server])
        .await
        .unwrap();
    world.session_id = Some(session.id().clone());
    world.session = Some(session);
}

#[when("I prompt the session")]
async fn prompt_session(world: &mut World) {
    let session = world.session.take().expect("the session is active");
    let session_id = session.id().clone();
    let mut events = world
        .events
        .take()
        .expect("the event receiver is available");
    let mut prompt =
        tokio::spawn(async move { session.prompt("integration test".to_owned()).await });

    loop {
        tokio::select! {
            result = &mut prompt => {
                result.unwrap().unwrap();
                break;
            }
            event = events.recv() => {
                if let Some(event) = event {
                    world.notifications.push(event);
                }
            }
        }
    }

    world.session_id = Some(session_id);
    world.events = Some(events);
}

#[then(regex = r#"^registered MCP tools mention "([^"]+)"$"#)]
async fn registered_mcp_tools_mention(world: &mut World, server_name: String) {
    let notification_text = format!("{:?}", world.notifications);
    assert!(notification_text.contains("Registered MCP tools:"));
    assert!(notification_text.contains(&server_name));
}

#[then("every notification belongs to the created session")]
async fn notifications_belong_to_created_session(world: &mut World) {
    let session_id = world.session_id.as_ref().expect("the session was created");
    assert!(!world.notifications.is_empty());
    assert!(
        world
            .notifications
            .iter()
            .all(|notification| &notification.session_id == session_id)
    );
}

#[when("I close and delete the session")]
async fn close_and_delete_session(world: &mut World) {
    let session = world.session.take().expect("the session is active");
    let closed = session.close().await.unwrap();
    let deleted: Session<SessionDeleted> = closed.delete().await.unwrap();
    world.deleted = true;
    drop(deleted);
}

#[when("I close the session")]
async fn close_session(world: &mut World) {
    let session = world.session.take().expect("the session is active");
    world.closed_session = Some(session.close().await.unwrap());
}

#[when("I try to delete the closed session")]
async fn try_delete_closed_session(world: &mut World) {
    let session = world.closed_session.take().expect("the session is closed");
    world.delete_rejected = session.delete().await.is_err();
}

#[when("I create two sessions")]
async fn create_two_sessions(world: &mut World) {
    let client = world.client.as_ref().expect("the connection is available");
    let first = client.new_session(Vec::new()).await.unwrap();
    let second = client.new_session(Vec::new()).await.unwrap();
    world.session_id = Some(first.id().clone());
    world.second_session_id = Some(second.id().clone());
    drop(first);
    drop(second);
}

#[then("the session is deleted")]
async fn session_is_deleted(world: &mut World) {
    assert!(world.deleted);
}

#[then("deleting the session is rejected")]
async fn deleting_session_is_rejected(world: &mut World) {
    assert!(world.delete_rejected);
}

#[then("the session IDs are distinct")]
async fn session_ids_are_distinct(world: &mut World) {
    assert_ne!(world.session_id, world.second_session_id);
}

#[then("the connection is disconnected")]
async fn disconnect_connection(world: &mut World) {
    let client = world.client.take().expect("the connection is active");
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn session_lifecycle() {
    World::cucumber()
        .with_default_cli()
        .run("tests/session_lifecycle.feature")
        .await;
}
