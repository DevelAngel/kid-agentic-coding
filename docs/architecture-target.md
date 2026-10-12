# Target Architecture

This document describes the intended architecture of app2. It follows arc42 and
describes the target state rather than treating the current implementation as a
design constraint.

The `app` application is deprecated and is not part of the target architecture.

## 1. Introduction and Goals

app2's application module is the Composition Root: it initializes the channels between modules, then stays out of runtime interaction.
Runtime behavior is driven by the workflow and ACP modules communicating through those channels.

The architecture aims to:
- Keep workflow policy and lifecycle in the workflow module.
- Keep ACP transport and protocol lifecycle in the ACP adapter.
- Make tool availability and workflow completion explicit.
- Support temporary, focused workflows such as commit-fix without moving their
  policy into transport code.
- Make runtime behavior observable and testable through clear boundaries.

## 2. Architecture Constraints

- The implementation language is Rust.
- ACP is the boundary to the external agent process.
- MCP tool-server integration is an intended capability. Its configuration and lifecycle belong to the responsible integration module, wired at startup.
- Type-state APIs should make invalid lifecycle transitions difficult to express,
  without replacing runtime error handling.
- Source code, comments, and technical documentation use American English.

## 3. Context and Scope

### System context

app2::application only composes the system at startup: it initializes channels between modules and does not participate in runtime message flow.
Workflow and ACP modules handle runtime behavior through those channels; each remains responsible for its own policy and lifecycle.

~~~mermaid
flowchart LR
    User[User / stdin or caller] --> Workflow[Workflow subsystem]
    App[app2::application (Composition Root)] -. initializes channels at startup .-> Workflow[Workflow subsystem]
    App -. initializes channels at startup .-> ACP[ACP adapter]
    Workflow <--> ACP
    ACP <--> Agent[External agent via ACP]
    Agent <--> MCP[MCP tool servers]
~~~

The application is a Composition Root, not a runtime coordinator. After initializing module channels, it has no role in user prompts, agent events, workflow transitions or shutdown.
The workflow subsystem and ACP adapter communicate through the initialized channels and own their respective responsibilities.

### In scope
- Startup composition and channel initialization.
- ACP connection and session lifecycle.
- Workflow definition, transitions, prompt context and completion.
- Tool availability and workflow-specific completion rules.
- Logging, errors and testable integration boundaries.

### Out of scope
- The internal implementation of external agents.
- The implementation details of individual MCP servers.
- The deprecated app application.

## 4. Solution Strategy

- **Workflow owns workflow policy.** Definitions, tool allowlists, workflow
  entry/exit and completion criteria live in the workflow subsystem.
- **ACP is an adapter.** It owns protocol connection details and message
  exchange, not workflow decisions or workflow identity.
- **The application is the Composition Root.** It initializes channels and wires modules together, then remains outside runtime message flow.
  Runtime coordination happens between the workflow and ACP modules over those channels.
- **Lifecycle is explicit.** Type states represent meaningful lifecycle stages;
  protocol failures remain fallible runtime outcomes.
- **Behavior is tested at boundaries.** Tests focus on observable prompts, tool
  availability, transitions and completion rather than private implementation
  types.

## 5. Building Block View

### Level 1: Application

~~~mermaid
flowchart TB
    App[Composition Root] -. initializes channels .-> Workflow[Workflow subsystem]
    App -. initializes channels .-> ACP[ACP adapter]
    Workflow --> Tools[Tool definitions and completion policy]
    ACP <--> Agent[External agent]
    Agent <--> MCP[MCP tool servers]
~~~

### Application entry and runtime

The `app2::application` module initializes the communication channels between runtime modules. It does not select workflows, forward prompts, route results or coordinate shutdown after initialization.

### Workflow subsystem

Owns workflow kinds, definitions, lifecycle, initial prompt context, permitted
tools, transitions to other workflows and completion policy. A workflow can
delegate a bounded task to another workflow and resume or finish according to
an explicit contract.

### Tool definitions and completion policy

Describes tools available in a workflow and the condition that marks that
workflow complete. Tool invocation transport is separate from the policy that
decides which tools are available and which result completes a workflow.

### ACP adapter

Owns ACP initialization, session creation, prompt requests, incoming protocol
events, connection failure propagation and disconnect. It does not own the
source of truth for workflow names, workflow tool names or workflow parameters.

### MCP integration

Owns the application-side configuration and lifecycle of MCP tool servers and
their exposure to the agent. Workflow-specific tool contracts remain owned by
the workflow subsystem.

## 6. Runtime View

### Startup and first prompt

1. The Composition Root initializes the channels connecting the workflow, ACP and tool modules.
2. Once initialization is complete, the application module has no further role in runtime behavior.
3. The ACP adapter launches or connects to the external agent, initializes the protocol connection and creates an ACP session in response to workflow requests.
4. The workflow subsystem starts the selected initial workflow and composes its initial context with the user's first prompt.
5. The workflow sends the composed prompt to ACP through the initialized channel.

### Subsequent prompts

1. The active workflow receives the next user prompt through its input channel.
2. The workflow applies its running-state prompt policy.

### Workflow transition and handoff

A running workflow can invoke a workflow-entry tool to delegate a bounded task to a transient workflow. The workflow subsystem defines the destination identity, entry and exit tool names, typed parameters, available tools and completion condition. It validates invocations and coordinates transitions through module channels; the application module is not involved. ACP transports agent interaction and owns session lifecycle.

~~~mermaid
sequenceDiagram
    participant CallerAgent as External agent (caller session)
    participant ACP as app2::acp
    participant Caller as app2::workflow (caller)
    participant Child as app2::workflow (transient)
    participant Tools as app2::tool
    participant ChildAgent as External agent (transient session)
    CallerAgent->>ACP: Invoke workflow-entry tool(parameters)
    ACP->>Caller: Deliver invocation over initialized channel
    Caller->>Caller: Validate against workflow contract
    Caller-->>Child: Transition request(task, typed parameters)
    Caller->>Caller: Preserve continuation state
    Child->>ACP: Request dedicated transient session
    ACP-->>Child: Transient session ready
    Child->>Tools: Read tool/completion contract
    Child->>ACP: Send focused prompt and transient tool contract
    ACP-->>ChildAgent: Continue transient workflow session
    ChildAgent->>ACP: Invoke transient workflow tools
    ACP->>Child: Deliver tool result over initialized channel
    Child->>Child: Evaluate completion condition
    Child-->>Caller: Successful completion(result)
    Child->>ACP: Close transient session
    Caller->>ACP: Send caller prompt and tool contract
    ACP-->>CallerAgent: Continue caller workflow
~~~

The handoff carries only the data required to execute and return from the
delegated task:

- **Destination workflow:** Identifies the workflow defined by the workflow subsystem.
- **Typed parameters:** Supplies validated, workflow-specific input.
- **Task and relevant context:** Explains the bounded work without copying the full conversation.
- **Caller continuation:** Preserves the calling workflow and the state needed to resume it.
- **Return contract:** Defines the result passed back to the caller and when control returns.
- **Tool and completion contract:** Defines the transient workflow's available tools and successful completion condition.


A transient workflow returns to its calling workflow only after its declared
completion condition succeeds. Its result is handed back to the preserved caller,
which resumes rather than being recreated. A failed tool invocation is not
successful completion and must not trigger the normal return transition.

The workflow subsystem owns workflow identity, typed entry parameters, entry and
exit tool names, tool availability, completion policy and return contract.
Each workflow runs in a dedicated ACP session. The ACP adapter owns session
lifecycle; session state is not part of the workflow handoff. The caller-preservation
and resume behavior described here is target architecture; it must not be mistaken
for behavior already implemented in the current code.

### Completion and shutdown

A workflow completes only when its declared completion condition is met.
The modules that own workflows and ACP sessions handle shutdown and release their resources. The application module does not coordinate runtime shutdown after channel initialization. Failures remain observable to the caller.

## 7. Deployment View

app2 runs as an application process and communicates with an external agent
process over ACP. The agent may expose MCP tools backed by separate processes
or services.

~~~mermaid
flowchart LR
    subgraph Host[Runtime environment]
        App[app2 process]
        Agent[External agent process]
        App <-->|ACP transport| Agent
    end
    Agent <-->|MCP transport| Servers[MCP servers / tools]
~~~

The concrete process-launch configuration, working directory, environment
variables and MCP server configuration are deployment concerns and should be
specified where the deployment setup is maintained.

## 8. Crosscutting Concepts

### Lifecycle and ownership

Connection, session and workflow lifecycles are separate concepts. Each subsystem
owns its own transitions and resources; similarly named lifecycle states must
not imply that one subsystem controls another.

### Errors

Expected failures are returned as Result values and propagated across boundaries
with enough context to diagnose the failing operation. Dropped channels or tasks
must not silently turn protocol failures into successful completion.

### Logging and observability

Use structured tracing for lifecycle changes, prompt exchange outcomes,
workflow transitions and errors. Avoid logging secrets or unnecessarily
sensitive prompt content.

### Testing

Prefer tests of observable behavior and contracts. Unit tests cover workflow
policy; integration tests cover workflow/ACP coordination through initialized channels and relevant agent-tool interactions.

## 9. Architecture Decisions

The following principles are part of the target architecture:
- Workflow policy is owned by the workflow subsystem.
- ACP is a protocol adapter, not the source of workflow metadata.
- Type states model lifecycle constraints, while runtime operations remain
  fallible.
- Workflow tool definitions and completion rules are explicit.

Any unresolved choice should be recorded as an open question rather than
presented as an established decision.

## 10. Quality Requirements

- **Modifiability:** Workflow policy can change without rewriting ACP transport.
- **Testability:** Workflow behavior and module-channel interactions can be tested at boundaries.
- **Reliability:** Connection, prompt and shutdown failures are propagated rather than hidden.
- **Understandability:** Module ownership and lifecycle transitions are explicit.
- **Observability:** Workflow transitions and protocol failures can be diagnosed from structured logs.

## 11. Risks and Technical Debt

The target is not considered complete until the implementation supports the
following end-to-end behavior:
- Workflow definitions are the authoritative source for workflow identity,
  entry/exit tool contracts and typed parameters.
- Workflow tool definitions are connected to the agent's actual tool invocation
  mechanism.
- MCP server configuration and lifecycle are integrated at the appropriate
  application boundary.
- ACP session lifecycle is represented by real protocol operations rather than
  type-state wrappers alone.
- Agent notifications and tool results are handled according to explicit
  contracts.
- End-to-end tests cover startup, prompt exchange, workflow transitions,
  completion and failure paths.

These are target-state checkpoints, not claims that every item is implemented.

## 12. Glossary

- **ACP:** Agent Client Protocol; the application-to-agent protocol boundary.
- **MCP:** Model Context Protocol; the protocol used to expose tools and context.
- **Workflow:** A bounded unit of work with a definition, lifecycle and
  completion policy.
- **Type state:** A Rust API design in which a type represents a lifecycle stage.
- **Completion tool:** A workflow-declared tool whose successful result can
  complete that workflow.
