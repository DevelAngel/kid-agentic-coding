# Architecture

`app2` is a typestate model of an ACP client, sessions, and workflows, with its own binary. `main.rs` is the only place where the client and the workflow meet.

## Overview

```mermaid
flowchart LR
    main["main.rs (binary app2)"] --> entry["ApplicationEntry"]
    main --> client["acp::Client"]
    entry --> workflow["Workflow K S"]
    workflow --> def["WorkflowDefinition"]
    def --> tools["ToolSet, Tool, CompletionTool"]
    result["ToolResult"] -.->|"input of try_complete"| workflow
    client --> session["acp::Session"]
    session --> mcp["McpServer"]
    client <-->|"ACP over stdio"| agent["Agent process"]
```

`Planning` has an initial definition but no prompt provider yet. `Session` is not tied to an ACP session, and nothing produces `ToolResult` values yet.

## Typestates

```mermaid
stateDiagram-v2
    direction LR
    [*] --> Disconnected: Client new with agent
    Disconnected --> Connected: connect()
    Connected --> Disconnected: disconnect()
```

```mermaid
stateDiagram-v2
    direction LR
    [*] --> Active: Client Connected new_session with mcp_servers
    Active --> Closed: close()
    Active --> Deleted: delete()
    Closed --> Deleted: delete()
```

```mermaid
stateDiagram-v2
    direction LR
    [*] --> Defined: initial() or transient()
    Defined --> Started: start()
    Started --> Running: run()
    Running --> Completed: try_complete, successful completion tool
    Running --> Running: try_complete, any other result
```

```mermaid
flowchart LR
    prog["Workflow Programming Running"] -->|"commit_fix()"| cf["Workflow CommitFix Running<br/>tools: git_status, git_diff, git_commit<br/>completes on successful git_commit"]
    cf -->|"try_complete with successful git_commit"| done["Workflow CommitFix Completed"]
```

## Channels

`Client::connect` spawns one connection task that owns the ACP connection. The caller side talks to it only through these channels.

```mermaid
flowchart LR
    caller["Client Connected"]
    task["connection task"]
    agent["Agent process"]

    caller -->|"prompt_tx: mpsc capacity 1<br/>prompt text + response oneshot"| task
    task -->|"response oneshot: Result PromptResponse"| caller
    caller -->|"shutdown: oneshot unit"| task
    task -->|"ready_tx: oneshot Result InitializeResponse"| caller
    task -->|"error_tx: oneshot Error"| caller
    task <-->|"ACP over stdio"| agent
```

| Channel | Kind | From → To | Payload | Notes |
|---|---|---|---|---|
| `ready_tx` | oneshot | task → `connect()` | `Result<InitializeResponse, Error>` | Sent after `initialize` and `session/new`. |
| `error_tx` | oneshot | task → `connect()` | `Error` | Read only when `ready_tx` was dropped, so the handshake error reaches the caller. |
| `shutdown` | oneshot | `Client<Connected>` → task | `()` | `disconnect()` drops the sender. |
| `prompt_tx` | mpsc, capacity 1 | `Client::prompt` → task | prompt text plus a response oneshot | The task handles one prompt at a time. |
| response oneshot | oneshot | task → `Client::prompt` | `Result<PromptResponse, Error>` | One per prompt. |

The task loop ends on `shutdown`, on `incoming_closed()` of the connection, or when `prompt_tx` is closed. Agent notifications are only logged.

## Sequences

### Connect and prompt

```mermaid
sequenceDiagram
    autonumber
    participant M as main
    participant C as Client
    participant T as connection task
    participant A as Agent

    M->>C: connect()
    C->>T: spawn with ready_tx, error_tx, shutdown_rx, prompt_rx
    T->>A: InitializeRequest V1
    A-->>T: InitializeResponse
    T->>A: NewSessionRequest (current dir)
    A-->>T: session_id
    T-->>C: ready_tx: Ok(InitializeResponse)

    loop prompts, one at a time
        M->>C: prompt(text)
        C->>T: prompt_tx: text with response oneshot
        T->>A: PromptRequest (session_id, text block)
        A-->>T: notifications, logged only
        A-->>T: PromptResponse (stop_reason)
        T-->>C: response oneshot
        C-->>M: PromptResponse
    end

    M->>C: disconnect()
    C->>T: drop shutdown sender
    T-->>C: JoinHandle result
```

### Failure paths

```mermaid
sequenceDiagram
    participant M as main
    participant C as Client
    participant T as connection task
    participant A as Agent

    alt handshake fails during connect
        T-->>C: error_tx: Error, ready_tx dropped
        C-->>M: connect returns Err
    else prompt request fails
        M->>C: prompt(text)
        C->>T: prompt_tx: text with response oneshot
        T->>A: PromptRequest
        A-->>T: Error
        T-->>C: response oneshot: Err
        Note over T: the task returns the error and ends
        C-->>M: prompt returns Err
    else agent connection closes
        Note over T: incoming_closed ends the loop, the task returns Ok
        M->>C: prompt(text)
        C-->>M: Err, prompt_tx closed or response oneshot dropped
    end
```

### Application loop

```mermaid
sequenceDiagram
    autonumber
    actor U as stdin
    participant M as main
    participant W as Workflow Programming
    participant C as Client

    M->>C: connect()
    M->>W: ApplicationEntry.workflow().start()
    U->>M: first line
    M->>W: prompt(line)
    W-->>M: workflow context plus line (Started)
    M->>C: prompt(text)
    M->>W: run()
    loop each further line
        U->>M: line
        M->>W: prompt(line)
        W-->>M: line unchanged (Running)
        M->>C: prompt(text)
    end
    U->>M: EOF
    M->>C: disconnect()
```
