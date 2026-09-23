use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use kid_agentic_coding_session::{
    AgentLauncher, FsSocketDir, SessionEvent, SessionHandle, StopReason,
};
use tokio::sync::oneshot;
use wire::{CloseAction, CloseActionVerdict, CommitFixRequest, CommitFixVerdict};

pub const MAIN_WORKFLOW: &str = "programming";
pub const COMMIT_FIX_WORKFLOW: &str = "commit-fix";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workflow {
    Main,
    OpeningFix,
    Fix,
}

impl Workflow {
    pub fn name(self) -> &'static str {
        match self {
            Self::Main => MAIN_WORKFLOW,
            Self::OpeningFix | Self::Fix => COMMIT_FIX_WORKFLOW,
        }
    }
}

#[derive(Clone)]
pub struct WorkflowView {
    state: Arc<AtomicU8>,
}

impl WorkflowView {
    pub fn new(workflow: Workflow) -> Self {
        let value = match workflow {
            Workflow::Main => 0,
            Workflow::OpeningFix => 1,
            Workflow::Fix => 2,
        };
        Self {
            state: Arc::new(AtomicU8::new(value)),
        }
    }

    pub fn workflow(&self) -> Workflow {
        match self.state.load(Ordering::Acquire) {
            0 => Workflow::Main,
            1 => Workflow::OpeningFix,
            _ => Workflow::Fix,
        }
    }

    pub fn name(&self) -> &'static str {
        self.workflow().name()
    }
}

pub struct WorkflowEvent {
    pub workflow: Workflow,
    pub event: SessionEvent,
}

pub struct WorkflowManager {
    main: SessionHandle,
    fix: Option<SessionHandle>,
    pending_fix: Option<CommitFixRequest>,
    /// Close-side Git action currently authorized and running.
    close_pending: Option<CloseAction>,
    view: WorkflowView,
}

impl WorkflowManager {
    pub fn new(main: SessionHandle) -> Self {
        Self {
            main,
            fix: None,
            pending_fix: None,
            close_pending: None,
            view: WorkflowView::new(Workflow::Main),
        }
    }

    pub fn view(&self) -> WorkflowView {
        self.view.clone()
    }

    fn set_workflow(&self, workflow: Workflow) {
        let value = match workflow {
            Workflow::Main => 0,
            Workflow::OpeningFix => 1,
            Workflow::Fix => 2,
        };
        self.view.state.store(value, Ordering::Release);
    }

    pub fn active_session(&self) -> &SessionHandle {
        self.fix.as_ref().unwrap_or(&self.main)
    }

    pub async fn recv_event(
        &mut self,
        launcher: &AgentLauncher,
        fs_socket_dir: FsSocketDir,
    ) -> Option<WorkflowEvent> {
        loop {
            let event = match self.fix.as_mut() {
                Some(fix) => {
                    tokio::select! {
                        event = self.main.recv_event() => {
                            event.map(|event| (Workflow::Main, event))
                        }
                        event = fix.recv_event() => {
                            event.map(|event| (Workflow::Fix, event))
                        }
                    }
                }
                None => self
                    .main
                    .recv_event()
                    .await
                    .map(|event| (Workflow::Main, event)),
            }?;

            let (workflow, event) = event;
            match (workflow, event) {
                (Workflow::Main, SessionEvent::CommitFix { request, verdict }) => {
                    self.request_fix(request, verdict);
                }
                (Workflow::Main, event) => {
                    if let Some(event) =
                        self.handle_main_event(event, launcher, fs_socket_dir.clone())
                    {
                        return Some(event);
                    }
                }
                (Workflow::Fix, SessionEvent::CloseAction { action, verdict }) => {
                    self.request_close_action(action, verdict);
                }
                (Workflow::Fix, event) => {
                    if let Some(event) = self.handle_fix_event(event) {
                        return Some(event);
                    }
                }
                (Workflow::OpeningFix, _) => {}
            }
        }
    }

    pub fn request_fix(
        &mut self,
        request: CommitFixRequest,
        verdict: oneshot::Sender<CommitFixVerdict>,
    ) {
        if self.view.workflow() != Workflow::Main {
            let _ = verdict.send(CommitFixVerdict::Ignored {
                reason: "a commit-fix workflow is already active".to_owned(),
            });
            return;
        }

        let _ = verdict.send(CommitFixVerdict::Opening);
        self.main.cancel();
        self.pending_fix = Some(request);
        self.set_workflow(Workflow::OpeningFix);
    }

    /// Gates a close-side Git action: only one attempt at a time, and only
    /// while a fix session is active.
    pub fn request_close_action(
        &mut self,
        action: CloseAction,
        verdict: oneshot::Sender<CloseActionVerdict>,
    ) {
        if self.view.workflow() != Workflow::Fix {
            let _ = verdict.send(CloseActionVerdict::Rejected {
                reason: "no fix session is active".to_owned(),
            });
            return;
        }
        if self.close_pending.is_some() {
            let _ = verdict.send(CloseActionVerdict::Rejected {
                reason: "another Git action is already being processed".to_owned(),
            });
            return;
        }

        self.close_pending = Some(action);
        let _ = verdict.send(CloseActionVerdict::Authorized);
    }

    fn handle_main_event(
        &mut self,
        event: SessionEvent,
        launcher: &AgentLauncher,
        fs_socket_dir: FsSocketDir,
    ) -> Option<WorkflowEvent> {
        if let SessionEvent::Stopped(reason) = &event
            && self.view.workflow() == Workflow::OpeningFix
            && matches!(reason, StopReason::Cancelled | StopReason::EndTurn)
            && let Some(request) = self.pending_fix.take()
        {
            return Some(self.start_fix(request, launcher, fs_socket_dir));
        }

        Some(WorkflowEvent {
            workflow: Workflow::Main,
            event,
        })
    }

    fn handle_fix_event(&mut self, event: SessionEvent) -> Option<WorkflowEvent> {
        match event {
            SessionEvent::CommitFixDone { commit_message } => {
                self.fix = None;
                self.close_pending = None;
                Some(self.send_prompt(
                    &self.main,
                    Workflow::Main,
                    format!("## Commit message used\n\n{commit_message}"),
                ))
            }
            SessionEvent::CloseActionOutcome { action, .. } => {
                if self.close_pending == Some(action) {
                    self.close_pending = None;
                } else {
                    // The session already logged the unexpected event.
                }
                None
            }
            event => Some(WorkflowEvent {
                workflow: Workflow::Fix,
                event,
            }),
        }
    }

    pub fn main_session(&self) -> &SessionHandle {
        &self.main
    }

    fn send_prompt(
        &self,
        session: &SessionHandle,
        workflow: Workflow,
        prompt: String,
    ) -> WorkflowEvent {
        self.set_workflow(workflow);
        let _ = session.send_prompt(&prompt);
        WorkflowEvent {
            workflow,
            event: SessionEvent::AutoPrompt(prompt),
        }
    }
    fn start_fix(
        &mut self,
        request: CommitFixRequest,
        launcher: &AgentLauncher,
        fs_socket_dir: FsSocketDir,
    ) -> WorkflowEvent {
        let CommitFixRequest {
            instructions,
            amend,
            tldr,
            why,
            what,
            cwd,
        } = request;

        let fix = launcher.start(
            true,
            Some(COMMIT_FIX_WORKFLOW.to_owned()),
            fs_socket_dir,
            cwd,
        );
        let amend_decision = if amend { "yes" } else { "no" };
        let seed_prompt = format!(
            "{instructions}\n\nCommit Amend Decision: {amend_decision}\n\n## TL;DR\n\n{tldr}\n\n## Why is this change needed?\n\n{why}\n\n## What does this change do?\n\n{what}"
        );
        let event = self.send_prompt(&fix, Workflow::Fix, seed_prompt);
        self.fix = Some(fix);
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn request() -> CommitFixRequest {
        CommitFixRequest {
            instructions: "fix it".to_owned(),
            amend: false,
            tldr: "short".to_owned(),
            why: "reason".to_owned(),
            what: "change".to_owned(),
            cwd: Some(PathBuf::from(".")),
        }
    }

    #[test]
    fn starts_in_main_workflow() {
        let manager = WorkflowManager::new(SessionHandle::new_disconnected_for_test());

        assert_eq!(manager.view().workflow(), Workflow::Main);
        assert_eq!(manager.view().name(), MAIN_WORKFLOW);
    }

    #[test]
    fn fix_prompt_is_emitted_as_an_auto_prompt() {
        let manager = WorkflowManager::new(SessionHandle::new_disconnected_for_test());
        let fix = SessionHandle::new_disconnected_for_test();

        let event = manager.send_prompt(&fix, Workflow::Main, "seed prompt".to_owned());
        assert_eq!(event.workflow, Workflow::Main);
        assert!(matches!(
            event.event,
            SessionEvent::AutoPrompt(prompt) if prompt == "seed prompt"
        ));
    }

    #[test]
    fn opening_is_non_blocking_state_transition() {
        let mut manager = WorkflowManager::new(SessionHandle::new_disconnected_for_test());
        let (verdict_tx, mut verdict_rx) = oneshot::channel();

        manager.request_fix(request(), verdict_tx);

        assert_eq!(verdict_rx.try_recv().ok(), Some(CommitFixVerdict::Opening));
        assert_eq!(manager.view().workflow(), Workflow::OpeningFix);
    }

    #[test]
    fn second_fix_request_is_ignored_without_replacing_state() {
        let mut manager = WorkflowManager::new(SessionHandle::new_disconnected_for_test());
        let (first_tx, mut first_rx) = oneshot::channel();
        manager.request_fix(request(), first_tx);
        let _ = first_rx.try_recv();

        let (second_tx, mut second_rx) = oneshot::channel();
        manager.request_fix(request(), second_tx);

        assert!(matches!(
            second_rx.try_recv().ok(),
            Some(CommitFixVerdict::Ignored { .. })
        ));
        assert_eq!(manager.view().workflow(), Workflow::OpeningFix);
    }

    /// Builds a manager already in the `Fix` workflow, for tests that gate
    /// close actions and don't exercise opening a real fix session.
    fn manager_in_fix_workflow() -> WorkflowManager {
        let mut manager = WorkflowManager::new(SessionHandle::new_disconnected_for_test());
        manager.fix = Some(SessionHandle::new_disconnected_for_test());
        manager.set_workflow(Workflow::Fix);
        manager
    }

    #[test]
    fn close_action_is_rejected_without_an_active_fix_session() {
        let mut manager = WorkflowManager::new(SessionHandle::new_disconnected_for_test());
        let (verdict_tx, mut verdict_rx) = oneshot::channel();

        manager.request_close_action(CloseAction::Add, verdict_tx);

        assert!(matches!(
            verdict_rx.try_recv().ok(),
            Some(CloseActionVerdict::Rejected { .. })
        ));
    }

    #[test]
    fn close_action_is_authorized_while_the_fix_session_is_active() {
        let mut manager = manager_in_fix_workflow();
        let (verdict_tx, mut verdict_rx) = oneshot::channel();

        manager.request_close_action(CloseAction::Add, verdict_tx);

        assert_eq!(
            verdict_rx.try_recv().ok(),
            Some(CloseActionVerdict::Authorized)
        );
    }

    #[test]
    fn a_second_close_action_is_rejected_while_one_is_still_pending() {
        let mut manager = manager_in_fix_workflow();
        let (first_tx, mut first_rx) = oneshot::channel();
        manager.request_close_action(CloseAction::Commit, first_tx);
        assert_eq!(
            first_rx.try_recv().ok(),
            Some(CloseActionVerdict::Authorized)
        );

        let (second_tx, mut second_rx) = oneshot::channel();
        manager.request_close_action(CloseAction::Commit, second_tx);

        assert!(matches!(
            second_rx.try_recv().ok(),
            Some(CloseActionVerdict::Rejected { .. })
        ));
    }

    #[test]
    fn close_action_outcome_releases_the_pending_gate_and_stays_in_fix() {
        let mut manager = manager_in_fix_workflow();
        let (first_tx, _first_rx) = oneshot::channel();
        manager.request_close_action(CloseAction::Add, first_tx);

        let outcome = manager.handle_fix_event(SessionEvent::CloseActionOutcome {
            action: CloseAction::Add,
            success: false,
            reason: Some("git add failed".to_owned()),
        });

        assert!(outcome.is_none());
        assert_eq!(manager.view().workflow(), Workflow::Fix);

        let (second_tx, mut second_rx) = oneshot::channel();
        manager.request_close_action(CloseAction::Add, second_tx);
        assert_eq!(
            second_rx.try_recv().ok(),
            Some(CloseActionVerdict::Authorized)
        );
    }

    #[test]
    fn unexpected_close_action_outcome_does_not_release_the_pending_gate() {
        let mut manager = manager_in_fix_workflow();
        let (first_tx, mut first_rx) = oneshot::channel();
        manager.request_close_action(CloseAction::Add, first_tx);
        assert_eq!(
            first_rx.try_recv().ok(),
            Some(CloseActionVerdict::Authorized)
        );

        let outcome = manager.handle_fix_event(SessionEvent::CloseActionOutcome {
            action: CloseAction::Commit,
            success: true,
            reason: None,
        });

        assert!(outcome.is_none());

        let (second_tx, mut second_rx) = oneshot::channel();
        manager.request_close_action(CloseAction::Commit, second_tx);
        assert!(matches!(
            second_rx.try_recv().ok(),
            Some(CloseActionVerdict::Rejected { .. })
        ));
    }
    #[test]
    fn commit_fix_done_also_releases_the_pending_gate() {
        let mut manager = manager_in_fix_workflow();
        let (commit_tx, _commit_rx) = oneshot::channel();
        manager.request_close_action(CloseAction::Commit, commit_tx);

        let event = manager
            .handle_fix_event(SessionEvent::CommitFixDone {
                commit_message: "fix: trim".to_owned(),
            })
            .expect("commit-fix-done is converted to an auto prompt");

        assert_eq!(event.workflow, Workflow::Main);
        assert!(matches!(
            event.event,
            SessionEvent::AutoPrompt(prompt)
                if prompt == "## Commit message used\n\nfix: trim"
        ));
        assert_eq!(manager.view().workflow(), Workflow::Main);
    }
}
