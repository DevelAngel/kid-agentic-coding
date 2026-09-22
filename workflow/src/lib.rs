use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use kid_agentic_coding_session::{
    AgentLauncher, FsSocketDir, SessionEvent, SessionHandle, StopReason,
};
use tokio::sync::oneshot;
use wire::{CommitFixRequest, CommitFixVerdict};

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
    view: WorkflowView,
}

impl WorkflowManager {
    pub fn new(main: SessionHandle) -> Self {
        Self {
            main,
            fix: None,
            pending_fix: None,
            view: WorkflowView::new(Workflow::Main),
        }
    }

    pub fn view(&self) -> WorkflowView {
        self.view.clone()
    }

    fn set_workflow(&mut self, workflow: Workflow) {
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
            self.start_fix(request, launcher, fs_socket_dir);
            return None;
        }

        Some(WorkflowEvent {
            workflow: Workflow::Main,
            event,
        })
    }

    fn handle_fix_event(&mut self, event: SessionEvent) -> Option<WorkflowEvent> {
        if let SessionEvent::CommitFixDone { commit_message } = event {
            self.fix = None;
            self.set_workflow(Workflow::Main);
            return Some(WorkflowEvent {
                workflow: Workflow::Main,
                event: SessionEvent::CommitFixDone { commit_message },
            });
        }

        Some(WorkflowEvent {
            workflow: Workflow::Fix,
            event,
        })
    }

    pub fn main_session(&self) -> &SessionHandle {
        &self.main
    }

    fn start_fix(
        &mut self,
        request: CommitFixRequest,
        launcher: &AgentLauncher,
        fs_socket_dir: FsSocketDir,
    ) {
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

        let _ = fix.send_prompt(seed_prompt);
        self.fix = Some(fix);
        self.set_workflow(Workflow::Fix);
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
}
