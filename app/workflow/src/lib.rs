use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use kid_agentic_coding_session::{
    AgentLauncher, FsSocketDir, SessionEvent, SessionHandle, StopReason,
};
use tokio::sync::oneshot;
use wire::{CloseAction, CloseActionVerdict, CommitFixRequest, CommitFixVerdict};

pub const MAIN_WORKFLOW: &str = "programming";
const CLOSE_ACTION_TIMEOUT: Duration = Duration::from_secs(30);
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

pub enum WorkflowEvent {
    Session {
        workflow: Workflow,
        transition: Option<Workflow>,
        event: SessionEvent,
    },
    SessionEnded {
        workflow: Workflow,
    },
}

pub struct WorkflowManager {
    main_ended: bool,
    main: SessionHandle,
    fix: Option<SessionHandle>,
    pending_fix: Option<CommitFixRequest>,
    /// Close-side Git action currently authorized and running.
    close_pending: Option<(CloseAction, Instant)>,
    view: WorkflowView,
}

impl WorkflowManager {
    pub fn new(main: SessionHandle) -> Self {
        Self {
            main,
            fix: None,
            main_ended: false,
            pending_fix: None,
            close_pending: None,
            view: WorkflowView::new(Self::initial_workflow()),
        }
    }

    pub fn view(&self) -> WorkflowView {
        self.view.clone()
    }

    pub fn initial_workflow() -> Workflow {
        Workflow::Main
    }

    fn set_workflow(&self, workflow: Workflow) -> Option<Workflow> {
        let value = match workflow {
            Workflow::Main => 0,
            Workflow::OpeningFix => 1,
            Workflow::Fix => 2,
        };
        let previous = self.view.state.swap(value, Ordering::AcqRel);
        (previous != value).then_some(workflow)
    }

    fn handle_session_end(&mut self, workflow: Workflow) -> WorkflowEvent {
        match workflow {
            Workflow::Main => {
                self.main_ended = true;
                self.pending_fix = None;
            }
            Workflow::Fix => {
                self.fix = None;
                self.close_pending = None;
                self.set_workflow(Workflow::Main);
            }
            Workflow::OpeningFix => {}
        }

        WorkflowEvent::SessionEnded { workflow }
    }

    pub fn active_session(&self) -> &SessionHandle {
        self.fix.as_ref().unwrap_or(&self.main)
    }

    pub async fn recv_event(
        &mut self,
        launcher: &AgentLauncher,
        fs_socket_dir: FsSocketDir,
    ) -> WorkflowEvent {
        loop {
            let close_action_deadline = self.close_pending.map(|(_, deadline)| deadline);
            let close_action_timeout = async {
                match close_action_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                    None => std::future::pending().await,
                }
            };
            let (workflow, event, close_action_expired) = match self.fix.as_mut() {
                Some(fix) => tokio::select! {
                    event = self.main.recv_event(), if !self.main_ended => {
                        (Workflow::Main, event, false)
                    }
                    event = fix.recv_event() => (Workflow::Fix, event, false),
                    _ = close_action_timeout => (Workflow::Fix, None, true),
                },
                None if self.main_ended => std::future::pending().await,
                None => (Workflow::Main, self.main.recv_event().await, false),
            };
            if close_action_expired {
                self.expire_close_action(Instant::now());
                continue;
            }

            let Some(event) = event else {
                return self.handle_session_end(workflow);
            };

            match (workflow, event) {
                (Workflow::Main, SessionEvent::CommitFix { request, verdict }) => {
                    self.request_fix(request, verdict);
                }
                (Workflow::Main, event) => {
                    if let Some(event) =
                        self.handle_main_event(event, launcher, fs_socket_dir.clone())
                    {
                        return event;
                    }
                }
                (Workflow::Fix, SessionEvent::CloseAction { action, verdict }) => {
                    self.request_close_action(action, verdict);
                }
                (Workflow::Fix, event) => {
                    if let Some(event) = self.handle_fix_event(event) {
                        return event;
                    }
                }
                (Workflow::OpeningFix, _) => unreachable!("opening state cannot own a session"),
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

        self.close_pending = Some((action, Instant::now() + CLOSE_ACTION_TIMEOUT));
        let _ = verdict.send(CloseActionVerdict::Authorized);
    }

    fn expire_close_action(&mut self, now: Instant) -> bool {
        if self
            .close_pending
            .is_some_and(|(_, deadline)| deadline <= now)
        {
            self.close_pending = None;
            return true;
        }
        false
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

        Some(WorkflowEvent::Session {
            workflow: Workflow::Main,
            transition: None,
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
                if self
                    .close_pending
                    .is_some_and(|(pending, _)| pending == action)
                {
                    self.close_pending = None;
                } else {
                    // The session already logged the unexpected event.
                }
                None
            }
            event => Some(WorkflowEvent::Session {
                workflow: Workflow::Fix,
                transition: None,
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
        let transition = self.set_workflow(workflow);
        let _ = session.send_prompt(&prompt);
        WorkflowEvent::Session {
            workflow,
            event: SessionEvent::AutoPrompt(prompt),
            transition,
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

        let WorkflowEvent::Session {
            workflow,
            transition,
            event,
        } = event
        else {
            panic!("expected session event");
        };
        assert_eq!(workflow, Workflow::Main);
        assert_eq!(transition, None);
        assert!(matches!(
            event,
            SessionEvent::AutoPrompt(prompt) if prompt == "seed prompt"
        ));
    }

    #[test]
    fn workflow_transition_is_attached_to_the_emitted_event() {
        let manager = WorkflowManager::new(SessionHandle::new_disconnected_for_test());
        let fix = SessionHandle::new_disconnected_for_test();

        assert!(matches!(
            manager.send_prompt(&fix, Workflow::Fix, "seed prompt".to_owned()),
            WorkflowEvent::Session {
                transition: Some(Workflow::Fix),
                ..
            }
        ));

        assert!(matches!(
            manager.send_prompt(&manager.main, Workflow::Main, "back to main".to_owned()),
            WorkflowEvent::Session {
                transition: Some(Workflow::Main),
                ..
            }
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
    fn fix_session_end_returns_to_main() {
        let mut manager = manager_in_fix_workflow();

        assert!(matches!(
            manager.handle_session_end(Workflow::Fix),
            WorkflowEvent::SessionEnded {
                workflow: Workflow::Fix
            }
        ));
        assert_eq!(manager.view().workflow(), Workflow::Main);
        assert!(manager.fix.is_none());
        assert!(manager.active_session().workflow_name().is_none());
    }

    #[test]
    fn main_session_end_is_recorded_without_replacing_the_session() {
        let mut manager = WorkflowManager::new(SessionHandle::new_disconnected_for_test());

        assert!(matches!(
            manager.handle_session_end(Workflow::Main),
            WorkflowEvent::SessionEnded {
                workflow: Workflow::Main
            }
        ));
        assert!(manager.main_ended);
        assert!(manager.fix.is_none());
        assert_eq!(manager.view().workflow(), Workflow::Main);
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
    fn expired_close_action_releases_the_pending_gate() {
        let mut manager = manager_in_fix_workflow();
        manager.close_pending = Some((CloseAction::Add, Instant::now() - Duration::from_secs(1)));

        assert!(manager.expire_close_action(Instant::now()));
        assert!(manager.close_pending.is_none());
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

        assert!(matches!(
            event,
            WorkflowEvent::Session {
                workflow: Workflow::Main,
                event: SessionEvent::AutoPrompt(prompt),
                ..
            } if prompt == "## Commit message used\n\nfix: trim"
        ));
        assert_eq!(manager.view().workflow(), Workflow::Main);
    }
}
