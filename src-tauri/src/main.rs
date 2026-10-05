// Prevents an additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // First statement on purpose: tao only sets Per-Monitor-V2 inside EventLoop::new, and any
    // window geometry read before that would be DPI-virtualized.
    #[cfg(windows)]
    borderfit_lib::win32::windows::set_dpi_awareness_early();

    borderfit_lib::run();
}
