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

`Client::connect` spawns one connection task that owns the ACP connection. Caller and task share five channels. Each channel has a sender end on one side and a receiver end on the other.

```mermaid
flowchart LR
    subgraph caller["Caller side: connect() and Client Connected"]
        prompt_tx["prompt_tx (Sender)"]
        response_rx["response_rx (Receiver)"]
        shutdown_tx["shutdown (Sender)"]
        ready_rx["ready_rx (Receiver)"]
        error_rx["error_rx (Receiver)"]
    end

    subgraph task["Connection task"]
        prompt_rx["prompt_rx (Receiver)"]
        response_tx["response_tx (Sender)"]
        shutdown_rx["shutdown_rx (Receiver)"]
        ready_tx["ready_tx (Sender)"]
        error_tx["error_tx (Sender)"]
    end

    prompt_tx ==>|"MPSC: many messages, capacity 1"| prompt_rx
    prompt_rx -.->|"each message carries the sender"| response_tx
    response_tx -->|"ONESHOT: one result per prompt"| response_rx
    shutdown_tx -->|"ONESHOT: stop signal"| shutdown_rx
    ready_tx -->|"ONESHOT: handshake result"| ready_rx
    error_tx -->|"ONESHOT: task error"| error_rx
```

Thick arrow: `mpsc`, many messages over time. Thin arrow: `oneshot`, exactly one message, then closed. Dotted arrow: not a channel. It shows that `response_tx` arrives inside the prompt message, so every prompt gets its own `oneshot`.

| Kind | Messages | Ends | Used for |
|---|---|---|---|
| `mpsc` | many, buffered | many senders, one receiver. Only one sender exists here, so it behaves like SPSC. | `prompt_tx` |
| `oneshot` | exactly one | one sender, one receiver. A sender dropped without sending makes the receiver return an error. | the other four |

`broadcast`, `watch`, and MPMC channels are not used.

| Channel | Carries | Notes |
|---|---|---|
| `prompt_tx` | prompt text plus a fresh `response_tx` | Capacity 1, so `prompt()` waits while the task is busy. One prompt at a time. |
| response | `Result<PromptResponse, Error>` | New per prompt. |
| `shutdown` | `()` | `disconnect()` drops the sender. |
| ready | `Result<InitializeResponse, Error>` | Sent after `initialize` and `session/new`. `connect()` waits on it. |
| error | `Error` | Read only when `ready_tx` was dropped, so the handshake error reaches the caller. |

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
