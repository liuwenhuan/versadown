use alldownloader_lib::{
    manager_close_disposition, ApplicationLifecycle, ManagerCloseDisposition,
};

#[test]
fn main_close_hides_until_application_exit_begins() {
    assert_eq!(
        manager_close_disposition("main", false),
        ManagerCloseDisposition::Hide
    );
    assert_eq!(
        manager_close_disposition("main", true),
        ManagerCloseDisposition::Exit
    );
    assert_eq!(
        manager_close_disposition("launcher", false),
        ManagerCloseDisposition::Exit
    );
}

#[test]
fn lifecycle_marks_only_intentional_application_exit() {
    let lifecycle = ApplicationLifecycle::new();

    assert!(!lifecycle.is_exiting());
    lifecycle.begin_exit();
    assert!(lifecycle.is_exiting());
}
