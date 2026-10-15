use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use agent_client_protocol::AcpAgent;
use cucumber::{World as _, given, then, when};
use kid_agentic_coding_app2::{Client, ClientConnected, Session, SessionActive, ToolEvent};
use tokio::sync::mpsc;
use tokio::time::timeout;

const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default, cucumber::World)]
struct World {
    client: Option<Client<ClientConnected>>,
    sessions: HashMap<String, Session<SessionActive>>,
    sinks: HashMap<String, mpsc::Receiver<ToolEvent>>,
    received: HashMap<String, Vec<ToolEvent>>,
}

impl fmt::Debug for World {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("World")
            .field("connected", &self.client.is_some())
            .field("sessions", &self.sessions.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl World {
    /// Waits until the named session has received an event matching `predicate`.
    async fn await_event(&mut self, name: &str, predicate: impl Fn(&ToolEvent) -> bool) -> bool {
        let received = self.received.entry(name.to_owned()).or_default();
        if received.iter().any(&predicate) {
            return true;
        }
        let sink = self.sinks.get_mut(name).expect("the session is routed");
        while let Ok(Some(event)) = timeout(DELIVERY_TIMEOUT, sink.recv()).await {
            let matched = predicate(&event);
            received.push(event);
            if matched {
                return true;
            }
        }
        false
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

async fn connect(world: &mut World, extra: &[&str]) {
    let agent = AcpAgent::from_args(agent_args(extra)).unwrap();
    let (events, _raw_events) = mpsc::channel(32);
    world.client = Some(Client::new(agent).connect(events).await.unwrap());
}

#[given("a lorem-agent connection")]
async fn connected(world: &mut World) {
    connect(world, &[]).await;
}

#[given("a lorem-agent connection whose first tool call fails")]
async fn connected_with_failing_tool(world: &mut World) {
    connect(world, &["--fail-tool"]).await;
}

#[given(regex = r#"^a routed session "([^"]+)"$"#)]
async fn routed_session(world: &mut World, name: String) {
    let client = world.client.as_ref().expect("the connection is available");
    let session = client.new_session(Vec::new()).await.unwrap();
    let (sink, receiver) = mpsc::channel(32);
    client.route(&session, sink);
    world.sessions.insert(name.clone(), session);
    world.sinks.insert(name, receiver);
}

#[when(regex = r#"^I prompt session "([^"]+)"$"#)]
async fn prompt_session(world: &mut World, name: String) {
    world
        .sessions
        .get(&name)
        .expect("the session exists")
        .prompt("integration test".to_owned())
        .await
        .unwrap();
}

#[then(regex = r#"^session "([^"]+)" received a call of tool "([^"]+)" with its input$"#)]
async fn received_call_with_input(world: &mut World, name: String, tool: String) {
    let found = world
        .await_event(&name, |event| {
            matches!(
                event,
                ToolEvent::Call(call)
                    if call.tool().name() == tool && call.input()["tool"] == tool.as_str()
            )
        })
        .await;
    assert!(found, "no call of {tool} with input reached {name}");
}

#[then(regex = r#"^session "([^"]+)" received a successful result of tool "([^"]+)"$"#)]
async fn received_successful_result(world: &mut World, name: String, tool: String) {
    let found = world
        .await_event(&name, |event| {
            matches!(
                event,
                ToolEvent::Result(result)
                    if result.tool().name() == tool && result.is_success()
            )
        })
        .await;
    assert!(found, "no successful result of {tool} reached {name}");
}

#[then(regex = r#"^session "([^"]+)" received a failed result of tool "([^"]+)"$"#)]
async fn received_failed_result(world: &mut World, name: String, tool: String) {
    let found = world
        .await_event(&name, |event| {
            matches!(
                event,
                ToolEvent::Result(result)
                    if result.tool().name() == tool && !result.is_success()
            )
        })
        .await;
    assert!(found, "no failed result of {tool} reached {name}");
}

#[then(regex = r#"^session "([^"]+)" received no tool events$"#)]
async fn received_no_events(world: &mut World, name: String) {
    let sink = world.sinks.get_mut(&name).expect("the session is routed");
    assert!(sink.try_recv().is_err(), "unexpected event reached {name}");
}

#[tokio::test]
async fn tool_delivery() {
    World::cucumber()
        .with_default_cli()
        .run("tests/tool_delivery.feature")
        .await;
}
