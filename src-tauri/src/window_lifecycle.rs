use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tauri::{WebviewWindow, WindowEvent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagerCloseDisposition {
    Hide,
    Exit,
}

pub fn manager_close_disposition(
    window_label: &str,
    application_exiting: bool,
) -> ManagerCloseDisposition {
    if window_label == "main" && !application_exiting {
        ManagerCloseDisposition::Hide
    } else {
        ManagerCloseDisposition::Exit
    }
}

pub struct ApplicationLifecycle {
    exiting: AtomicBool,
}

impl ApplicationLifecycle {
    pub fn new() -> Self {
        Self {
            exiting: AtomicBool::new(false),
        }
    }

    pub fn begin_exit(&self) {
        self.exiting.store(true, Ordering::Release);
    }

    pub fn is_exiting(&self) -> bool {
        self.exiting.load(Ordering::Acquire)
    }
}

impl Default for ApplicationLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

pub fn install_manager_close_handler(
    window: &WebviewWindow,
    lifecycle: Arc<ApplicationLifecycle>,
) {
    let manager = window.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::CloseRequested { .. })
            && manager_close_disposition(manager.label(), lifecycle.is_exiting())
                == ManagerCloseDisposition::Hide
        {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = manager.hide();
            }
        }
    });
}
