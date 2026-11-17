//! Engineering-workflow state machine.
//!
//! This is application workflow state, *not* A2A task state: an A2A task
//! reaching `completed` only means an agent returned an artifact; whether the
//! workflow advances is decided here (and by artifact validation and gates).
//!
//! Rules enforced by [`next`]:
//! * every event has exactly one kind of actor allowed to cause it;
//! * human gates (plan approval, delivery decision) require a human with the
//!   `approver` role; no agent role and not the system can cause them;
//! * terminal states never change;
//! * revision loops and agent runs are bounded by [`Budget`];
//! * every accepted transition increments `version` (optimistic concurrency:
//!   a decision made against an older version is stale).

use serde::{Deserialize, Serialize};

use crate::roles::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Project Manager is producing the plan.
    Draft,
    AwaitingPlanApproval,
    /// Plan approved; Coordinator is creating the work assignment.
    ApprovedForExecution,
    /// Developer is implementing in the isolated workspace.
    Executing,
    /// Code Reviewer is reviewing the exact candidate commit.
    Reviewing,
    /// QA Tester and then Quality Lead work on the same candidate.
    Testing,
    AwaitingDeliveryApproval,
    /// Human asked for changes; Coordinator re-assigns (bounded).
    ChangesRequested,
    ApprovedForDelivery,
    Rejected,
    Failed,
    Cancelled,
}

impl Status {
    pub const ALL: [Status; 12] = [
        Status::Draft,
        Status::AwaitingPlanApproval,
        Status::ApprovedForExecution,
        Status::Executing,
        Status::Reviewing,
        Status::Testing,
        Status::AwaitingDeliveryApproval,
        Status::ChangesRequested,
        Status::ApprovedForDelivery,
        Status::Rejected,
        Status::Failed,
        Status::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::AwaitingPlanApproval => "awaiting_plan_approval",
            Status::ApprovedForExecution => "approved_for_execution",
            Status::Executing => "executing",
            Status::Reviewing => "reviewing",
            Status::Testing => "testing",
            Status::AwaitingDeliveryApproval => "awaiting_delivery_approval",
            Status::ChangesRequested => "changes_requested",
            Status::ApprovedForDelivery => "approved_for_delivery",
            Status::Rejected => "rejected",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Status::ApprovedForDelivery | Status::Rejected | Status::Failed | Status::Cancelled
        )
    }

    /// Waiting for a human; nothing is running, so a restart preserves it.
    pub fn is_human_gate(self) -> bool {
        matches!(
            self,
            Status::AwaitingPlanApproval | Status::AwaitingDeliveryApproval
        )
    }

    /// An agent stage was (or was about to be) in flight.
    pub fn is_agent_active(self) -> bool {
        !self.is_terminal() && !self.is_human_gate()
    }

    /// The role whose run moves the workflow out of this state, if any.
    pub fn active_role(self) -> Option<Role> {
        match self {
            Status::Draft => Some(Role::ProjectManager),
            Status::ApprovedForExecution | Status::ChangesRequested => {
                Some(Role::ProjectCoordinator)
            }
            Status::Executing => Some(Role::Developer),
            Status::Reviewing => Some(Role::CodeReviewer),
            // QA first, then the Quality Lead; the orchestrator sequences both.
            Status::Testing => Some(Role::QaTester),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    PlanProposed,
    PlanApproved,
    PlanChangesRequested,
    WorkAssigned,
    CandidateProduced,
    ReviewRecorded,
    EvidenceAssessed,
    DeliveryApproved,
    DeliveryChangesRequested,
    DeliveryRejected,
    Cancel,
    Fail,
}

impl Event {
    pub fn as_str(self) -> &'static str {
        match self {
            Event::PlanProposed => "plan_proposed",
            Event::PlanApproved => "plan_approved",
            Event::PlanChangesRequested => "plan_changes_requested",
            Event::WorkAssigned => "work_assigned",
            Event::CandidateProduced => "candidate_produced",
            Event::ReviewRecorded => "review_recorded",
            Event::EvidenceAssessed => "evidence_assessed",
            Event::DeliveryApproved => "delivery_approved",
            Event::DeliveryChangesRequested => "delivery_changes_requested",
            Event::DeliveryRejected => "delivery_rejected",
            Event::Cancel => "cancel",
            Event::Fail => "fail",
        }
    }

    fn required(self) -> Required {
        match self {
            Event::PlanProposed => Required::Agent(Role::ProjectManager),
            Event::WorkAssigned => Required::Agent(Role::ProjectCoordinator),
            Event::CandidateProduced => Required::Agent(Role::Developer),
            Event::ReviewRecorded => Required::Agent(Role::CodeReviewer),
            Event::EvidenceAssessed => Required::Agent(Role::QualityLead),
            Event::PlanApproved
            | Event::PlanChangesRequested
            | Event::DeliveryApproved
            | Event::DeliveryChangesRequested
            | Event::DeliveryRejected => Required::Human(HumanRole::Approver),
            Event::Cancel => Required::AnyHuman,
            Event::Fail => Required::System,
        }
    }
}

/// Server-side role of an authenticated human (from the identity provider,
/// never from the request body).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanRole {
    /// May submit requirements and cancel own workflows.
    Requester,
    /// May decide plan and delivery gates.
    Approver,
}

impl HumanRole {
    pub fn as_str(self) -> &'static str {
        match self {
            HumanRole::Requester => "requester",
            HumanRole::Approver => "approver",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Human { id: String, roles: Vec<HumanRole> },
    Agent { role: Role, agent_id: String },
    System,
}

impl Actor {
    pub fn kind(&self) -> &'static str {
        match self {
            Actor::Human { .. } => "human",
            Actor::Agent { .. } => "agent",
            Actor::System => "system",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Actor::Human { id, .. } => id,
            Actor::Agent { agent_id, .. } => agent_id,
            Actor::System => "orchestrator",
        }
    }
}

enum Required {
    Agent(Role),
    Human(HumanRole),
    AnyHuman,
    System,
}

impl Required {
    fn allows(&self, actor: &Actor) -> bool {
        match (self, actor) {
            (Required::Agent(r), Actor::Agent { role, .. }) => r == role,
            (Required::Human(r), Actor::Human { roles, .. }) => roles.contains(r),
            (Required::AnyHuman, Actor::Human { .. }) => true,
            (Required::System, Actor::System) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub max_plan_revisions: u32,
    pub max_delivery_revisions: u32,
    pub max_agent_runs: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            max_plan_revisions: 2,
            max_delivery_revisions: 2,
            max_agent_runs: 24,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkflowState {
    pub status: Status,
    pub version: i64,
    pub plan_revisions: u32,
    pub delivery_revisions: u32,
    pub agent_runs: u32,
}

impl WorkflowState {
    pub fn new() -> Self {
        WorkflowState {
            status: Status::Draft,
            version: 1,
            plan_revisions: 0,
            delivery_revisions: 0,
            agent_runs: 0,
        }
    }
}

impl Default for WorkflowState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    #[error("workflow is {0:?} (terminal); no further transitions")]
    Terminal(Status),
    #[error("event `{}` is not valid in state `{}`", .event.as_str(), .from.as_str())]
    NotAllowed { from: Status, event: Event },
    #[error("{actor} is not authorized to cause `{}`", .event.as_str())]
    Unauthorized { event: Event, actor: &'static str },
    #[error("budget exceeded: {0}")]
    BudgetExceeded(&'static str),
}

/// Computes the next state. Pure: the caller persists it atomically with a
/// version check and an audit record.
pub fn next(
    s: &WorkflowState,
    event: Event,
    actor: &Actor,
    budget: &Budget,
) -> Result<WorkflowState, TransitionError> {
    use Status::*;
    if s.status.is_terminal() {
        return Err(TransitionError::Terminal(s.status));
    }
    if !event.required().allows(actor) {
        return Err(TransitionError::Unauthorized {
            event,
            actor: actor.kind(),
        });
    }
    let mut n = *s;
    n.status = match (s.status, event) {
        (Draft, Event::PlanProposed) => AwaitingPlanApproval,
        (AwaitingPlanApproval, Event::PlanApproved) => ApprovedForExecution,
        (AwaitingPlanApproval, Event::PlanChangesRequested) => {
            if s.plan_revisions >= budget.max_plan_revisions {
                return Err(TransitionError::BudgetExceeded("plan revisions"));
            }
            n.plan_revisions += 1;
            Draft
        }
        (ApprovedForExecution, Event::WorkAssigned) => Executing,
        (ChangesRequested, Event::WorkAssigned) => Executing,
        (Executing, Event::CandidateProduced) => Reviewing,
        (Reviewing, Event::ReviewRecorded) => Testing,
        (Testing, Event::EvidenceAssessed) => AwaitingDeliveryApproval,
        (AwaitingDeliveryApproval, Event::DeliveryApproved) => ApprovedForDelivery,
        (AwaitingDeliveryApproval, Event::DeliveryRejected) => Rejected,
        (AwaitingDeliveryApproval, Event::DeliveryChangesRequested) => {
            if s.delivery_revisions >= budget.max_delivery_revisions {
                return Err(TransitionError::BudgetExceeded("delivery revisions"));
            }
            n.delivery_revisions += 1;
            ChangesRequested
        }
        (_, Event::Cancel) => Cancelled,
        // Nothing runs while waiting for a human, so nothing there can fail.
        (from, Event::Fail) if from.is_agent_active() => Failed,
        (from, event) => return Err(TransitionError::NotAllowed { from, event }),
    };
    n.version += 1;
    Ok(n)
}

/// Reserves one agent run against the budget (retries count too).
pub fn reserve_agent_run(
    s: &WorkflowState,
    budget: &Budget,
) -> Result<WorkflowState, TransitionError> {
    if s.agent_runs >= budget.max_agent_runs {
        return Err(TransitionError::BudgetExceeded("agent runs"));
    }
    let mut n = *s;
    n.agent_runs += 1;
    Ok(n)
}

/// Restart recovery rule: a stage that was in flight is failed (`interrupted`,
/// no silent replay, as in P0); a human gate is preserved untouched.
pub fn recovery_event(status: Status) -> Option<Event> {
    status.is_agent_active().then_some(Event::Fail)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn agent(role: Role) -> Actor {
        Actor::Agent {
            role,
            agent_id: format!("{}-1", role.as_str()),
        }
    }
    fn approver() -> Actor {
        Actor::Human {
            id: "alice".into(),
            roles: vec![HumanRole::Approver],
        }
    }
    fn requester() -> Actor {
        Actor::Human {
            id: "bob".into(),
            roles: vec![HumanRole::Requester],
        }
    }

    fn run(events: &[(Event, Actor)]) -> Result<WorkflowState, TransitionError> {
        let b = Budget::default();
        let mut s = WorkflowState::new();
        for (e, a) in events {
            s = next(&s, *e, a, &b)?;
        }
        Ok(s)
    }

    fn happy_until_delivery() -> Vec<(Event, Actor)> {
        vec![
            (Event::PlanProposed, agent(Role::ProjectManager)),
            (Event::PlanApproved, approver()),
            (Event::WorkAssigned, agent(Role::ProjectCoordinator)),
            (Event::CandidateProduced, agent(Role::Developer)),
            (Event::ReviewRecorded, agent(Role::CodeReviewer)),
            (Event::EvidenceAssessed, agent(Role::QualityLead)),
        ]
    }

    #[test]
    fn approved_flow_in_role_order() {
        let mut ev = happy_until_delivery();
        let s = run(&ev).unwrap();
        assert_eq!(s.status, Status::AwaitingDeliveryApproval);
        ev.push((Event::DeliveryApproved, approver()));
        let s = run(&ev).unwrap();
        assert_eq!(s.status, Status::ApprovedForDelivery);
        assert_eq!(s.version, 8);
    }

    #[test]
    fn changes_requested_loops_back_and_is_bounded() {
        let b = Budget::default();
        let mut s = run(&happy_until_delivery()).unwrap();
        for i in 0..b.max_delivery_revisions {
            s = next(&s, Event::DeliveryChangesRequested, &approver(), &b).unwrap();
            assert_eq!(s.status, Status::ChangesRequested);
            assert_eq!(s.delivery_revisions, i + 1);
            for (e, a) in happy_until_delivery().into_iter().skip(2) {
                s = next(&s, e, &a, &b).unwrap();
            }
        }
        assert_eq!(
            next(&s, Event::DeliveryChangesRequested, &approver(), &b),
            Err(TransitionError::BudgetExceeded("delivery revisions"))
        );
    }

    #[test]
    fn no_agent_role_and_not_the_system_can_pass_a_human_gate() {
        let gate = run(&happy_until_delivery()).unwrap();
        let plan_gate = run(&happy_until_delivery()[..1]).unwrap();
        let b = Budget::default();
        let mut actors: Vec<Actor> = Role::ALL.into_iter().map(agent).collect();
        actors.push(Actor::System);
        actors.push(requester());
        for a in &actors {
            for e in [
                Event::DeliveryApproved,
                Event::DeliveryRejected,
                Event::DeliveryChangesRequested,
            ] {
                assert!(
                    matches!(
                        next(&gate, e, a, &b),
                        Err(TransitionError::Unauthorized { .. })
                    ),
                    "{a:?} {e:?}"
                );
            }
            for e in [Event::PlanApproved, Event::PlanChangesRequested] {
                assert!(matches!(
                    next(&plan_gate, e, a, &b),
                    Err(TransitionError::Unauthorized { .. })
                ));
            }
        }
    }

    #[test]
    fn stages_cannot_be_skipped_or_reordered() {
        let b = Budget::default();
        let s = WorkflowState::new();
        // Developer before plan approval, QA lead before review, approval before evidence.
        assert!(matches!(
            next(&s, Event::CandidateProduced, &agent(Role::Developer), &b),
            Err(TransitionError::NotAllowed { .. })
        ));
        let executing = run(&happy_until_delivery()[..3]).unwrap();
        assert!(matches!(
            next(
                &executing,
                Event::EvidenceAssessed,
                &agent(Role::QualityLead),
                &b
            ),
            Err(TransitionError::NotAllowed { .. })
        ));
        assert!(matches!(
            next(&executing, Event::DeliveryApproved, &approver(), &b),
            Err(TransitionError::NotAllowed { .. })
        ));
        // Wrong agent for the stage.
        assert!(matches!(
            next(
                &executing,
                Event::CandidateProduced,
                &agent(Role::CodeReviewer),
                &b
            ),
            Err(TransitionError::Unauthorized { .. })
        ));
    }

    #[test]
    fn terminal_states_are_final_and_failure_never_becomes_approval() {
        let b = Budget::default();
        let failed = next(
            &run(&happy_until_delivery()[..4]).unwrap(),
            Event::Fail,
            &Actor::System,
            &b,
        )
        .unwrap();
        assert_eq!(failed.status, Status::Failed);
        for e in [
            Event::DeliveryApproved,
            Event::EvidenceAssessed,
            Event::Cancel,
            Event::Fail,
        ] {
            assert_eq!(
                next(&failed, e, &approver(), &b),
                Err(TransitionError::Terminal(Status::Failed))
            );
        }
    }

    #[test]
    fn human_gates_cannot_fail_and_survive_restart() {
        let gate = run(&happy_until_delivery()).unwrap();
        assert!(matches!(
            next(&gate, Event::Fail, &Actor::System, &Budget::default()),
            Err(TransitionError::NotAllowed { .. })
        ));
        assert_eq!(recovery_event(Status::AwaitingDeliveryApproval), None);
        assert_eq!(recovery_event(Status::AwaitingPlanApproval), None);
        assert_eq!(recovery_event(Status::Executing), Some(Event::Fail));
        assert_eq!(recovery_event(Status::Rejected), None);
    }

    #[test]
    fn plan_revisions_are_bounded() {
        let b = Budget {
            max_plan_revisions: 1,
            ..Budget::default()
        };
        let mut s = next(
            &WorkflowState::new(),
            Event::PlanProposed,
            &agent(Role::ProjectManager),
            &b,
        )
        .unwrap();
        s = next(&s, Event::PlanChangesRequested, &approver(), &b).unwrap();
        assert_eq!(s.status, Status::Draft);
        s = next(&s, Event::PlanProposed, &agent(Role::ProjectManager), &b).unwrap();
        assert_eq!(
            next(&s, Event::PlanChangesRequested, &approver(), &b),
            Err(TransitionError::BudgetExceeded("plan revisions"))
        );
    }

    #[test]
    fn agent_run_budget() {
        let b = Budget {
            max_agent_runs: 2,
            ..Budget::default()
        };
        let s = reserve_agent_run(&WorkflowState::new(), &b).unwrap();
        let s = reserve_agent_run(&s, &b).unwrap();
        assert_eq!(
            reserve_agent_run(&s, &b),
            Err(TransitionError::BudgetExceeded("agent runs"))
        );
    }

    #[test]
    fn any_human_can_cancel_but_agents_cannot() {
        let s = run(&happy_until_delivery()[..3]).unwrap();
        let b = Budget::default();
        assert_eq!(
            next(&s, Event::Cancel, &requester(), &b).unwrap().status,
            Status::Cancelled
        );
        assert!(next(&s, Event::Cancel, &agent(Role::ProjectCoordinator), &b).is_err());
    }
}
