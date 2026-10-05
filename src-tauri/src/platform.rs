//! One place that picks the Win32 implementation on Windows and harmless stand-ins elsewhere,
//! so the Tauri glue (lib.rs, commands.rs) has no `cfg` noise.

#[cfg(not(windows))]
use crate::model::AppError;
use crate::win32::WindowManager;

#[cfg(windows)]
pub use crate::win32::windows::ShellMessage;

/// A fresh window manager (stateless, cheap).
pub fn window_manager() -> Box<dyn WindowManager> {
    #[cfg(windows)]
    return Box::new(crate::win32::windows::Win32WindowManager::new());
    #[cfg(not(windows))]
    return Box::new(crate::win32::fake::FakeWindowManager::default());
}

#[cfg(windows)]
mod imp {
    pub use crate::win32::windows::{
        beep, close_instance_event, create_instance_event, dpi_context_description as dpi_context,
        install_subclass, is_elevated, os_build, other_instance_running, relaunch_elevated,
        set_autostart, shell_open,
    };
}

#[cfg(not(windows))]
mod imp {
    use super::AppError;

    pub fn beep() {}
    pub fn close_instance_event() {}
    pub fn create_instance_event() {}
    pub fn dpi_context() -> String {
        "n/a".into()
    }
    pub fn is_elevated() -> bool {
        false
    }
    pub fn os_build() -> String {
        std::env::consts::OS.into()
    }
    pub fn other_instance_running() -> bool {
        false
    }
    pub fn relaunch_elevated() -> Result<(), AppError> {
        Err(AppError::win32("ShellExecuteExW", 50))
    }
    pub fn set_autostart(_enabled: bool) -> Result<(), AppError> {
        Ok(())
    }
    pub fn shell_open(target: &str) {
        log::info!("open {target}");
    }
}

pub use imp::*;
