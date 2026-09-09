use alldownloader_lib::{may_transition, TaskStatus};

#[test]
fn terminal_tasks_can_move_to_the_recycle_bin() {
    assert!(may_transition(TaskStatus::Completed, TaskStatus::Recycled));
    assert!(may_transition(TaskStatus::Failed, TaskStatus::Recycled));
    assert!(may_transition(TaskStatus::Cancelled, TaskStatus::Recycled));
}
