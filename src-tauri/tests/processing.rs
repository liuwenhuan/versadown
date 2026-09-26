use versadown_lib::{plan_processing, ProcessingHint, Resource};

#[test]
fn password_from_page_can_be_attached_without_being_serialized_in_trace() {
    let resource = Resource::archive("archive.7z");
    let plan = plan_processing(&[resource], &[ProcessingHint::password("abc")]);
    assert!(plan[0].password_reference.is_some());
    assert!(!format!("{:?}", plan[0]).contains("abc"));
}

#[test]
fn executable_resource_never_generates_a_processing_step() {
    let resource = Resource::file("setup.exe");
    assert!(plan_processing(&[resource], &[]).is_empty());
}
