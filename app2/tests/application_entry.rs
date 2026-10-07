use kid_agentic_coding_app2::{ApplicationEntry, Planning, Programming, Started, Workflow};

#[test]
fn programming_is_the_default_entry_workflow() {
    let workflow = ApplicationEntry::<Programming>::default().workflow();

    let _: Workflow<Programming, Started> = workflow.start();
}

#[test]
fn planning_can_be_selected_as_an_initial_workflow() {
    let workflow = ApplicationEntry::<Planning>::default().workflow();

    let _: Workflow<Planning, Started> = workflow.start();
}
