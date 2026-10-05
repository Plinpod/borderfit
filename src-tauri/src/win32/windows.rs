//! The Win32 implementation of [`WindowManager`] and the free functions the shell needs
//! (DPI awareness, elevation, relaunch, single-instance event, autostart, subclassing, links).
//!
//! Every `unsafe` block calls documented Win32 APIs with handles and buffers owned by the
//! surrounding function; invalid window handles make the calls fail, which is handled.

use super::{
    ProcessHandle, ShowCmd, TrayWindow, WindowInfo, WindowManager, ZOrder,
    ERROR_INVALID_WINDOW_HANDLE, PRIMARY_TRAY_CLASS, SECONDARY_TRAY_CLASS,
};
use crate::model::{AppError, DpiAwareness, MonitorInfo, Placement, Point, Rect};
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;
use windows::core::{w, BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND,
    ERROR_SUCCESS, FILETIME, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0,
    WIN32_ERROR, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CLOAKED,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DEFAULT, DWMWCP_DONOTROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenElevation,
    TokenIntegrityLevel, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ELEVATION,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Com::{
    CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenEventW, OpenProcess,
    OpenProcessToken, QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetAwarenessFromDpiAwarenessContext, GetDpiForMonitor,
    GetDpiForWindow, GetThreadDpiAwarenessContext, GetWindowDpiAwarenessContext,
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_PER_MONITOR_AWARE,
    DPI_AWARENESS_SYSTEM_AWARE, DPI_AWARENESS_UNAWARE, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, SetWindowSubclass, ShellExecuteExW, ShellExecuteW, SEE_MASK_NOASYNC,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, FindWindowExW, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetTopWindow, GetWindow, GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
    RegisterWindowMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPlacement,
    SetWindowPos, ShowWindow, ShowWindowAsync, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GW_HWNDNEXT,
    HWND_NOTOPMOST, HWND_TOPMOST, MB_ICONWARNING, MSGFLT_ALLOW, SET_WINDOW_POS_FLAGS,
    SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOCOPYBITS, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSENDCHANGING, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_RESTORE,
    SW_SHOWMINNOACTIVE, SW_SHOWNA, SW_SHOWNORMAL, WINDOWPLACEMENT, WINDOWPLACEMENT_FLAGS,
    WM_ENDSESSION, WM_QUERYENDSESSION,
};

/// DWMWA_COLOR_NONE / DWMWA_COLOR_DEFAULT for DWMWA_BORDER_COLOR.
const DWM_COLOR_NONE: u32 = 0xFFFF_FFFE;
const DWM_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;

fn to_hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn from_hwnd(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

/// Win32 error code carried by a windows-rs error (HRESULT_FROM_WIN32 unwrapped).
fn error_code(error: &windows::core::Error) -> u32 {
    let hr = error.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        hr & 0xFFFF
    } else {
        hr
    }
}

fn api_error(api: &str, error: &windows::core::Error) -> AppError {
    let code = error_code(error);
    if code == ERROR_INVALID_WINDOW_HANDLE {
        return AppError::WindowGone;
    }
    AppError::win32(api, code)
}

fn last_error() -> WIN32_ERROR {
    // SAFETY: reads the calling thread's last-error value.
    unsafe { GetLastError() }
}

fn wide_to_string(buffer: &[u16], len: usize) -> String {
    String::from_utf16_lossy(&buffer[..len.min(buffer.len())])
}

fn rect_from(r: RECT) -> Rect {
    Rect::from_edges(r.left, r.top, r.right, r.bottom)
}

fn rect_to(r: Rect) -> RECT {
    RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

/// An owned kernel handle, closed on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by this module and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open_process_query(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: plain OpenProcess; the handle is wrapped so it is always closed.
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok().map(OwnedHandle) }
}

/// File name of a process's executable, e.g. `explorer.exe` (empty if it can't be read).
pub fn process_exe_name(pid: u32) -> String {
    let Some(process) = open_process_query(pid) else { return String::new() };
    let mut buffer = [0u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: the buffer and its length describe valid writable memory.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    if ok.is_err() {
        return String::new();
    }
    let path = wide_to_string(&buffer, len as usize);
    path.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

fn process_creation_time(process: HANDLE) -> u64 {
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: four valid out-pointers to stack FILETIMEs.
    let ok = unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    if ok.is_ok() {
        filetime_to_u64(created)
    } else {
        0
    }
}

/// Integrity level RID (0x2000 medium, 0x3000 high, …) of a process token.
fn token_integrity_level(process: HANDLE) -> Option<u32> {
    let mut token = HANDLE::default();
    // SAFETY: valid out-pointer; the token handle is wrapped right after.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
    let token = OwnedHandle(token);
    let mut buffer = [0u8; 256];
    let mut needed = 0u32;
    // SAFETY: the buffer is large enough for a TOKEN_MANDATORY_LABEL plus its SID.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenIntegrityLevel,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            &mut needed,
        )
    }
    .ok()?;
    // SAFETY: GetTokenInformation filled the buffer with a TOKEN_MANDATORY_LABEL whose SID
    // pointer points into the same buffer; the sub-authority count bounds the index.
    unsafe {
        let label = &*(buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>());
        let sid = label.Label.Sid;
        let count = u32::from(*GetSidSubAuthorityCount(sid));
        if count == 0 {
            return None;
        }
        Some(*GetSidSubAuthority(sid, count - 1))
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: the slice is a valid writable buffer.
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    wide_to_string(&buffer, len.max(0) as usize)
}

fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 512];
    // SAFETY: the slice is a valid writable buffer; for other processes this reads the cached
    // caption without sending a message, so a hung window can't block it.
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    wide_to_string(&buffer, len.max(0) as usize)
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: a 4-byte out-buffer for DWMWA_CLOAKED.
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    ok.is_ok() && cloaked != 0
}

/// `IsWindowArranged` exists only on newer builds; a static import would stop the exe from
/// loading on Windows 10, so it is looked up at runtime.
fn is_arranged(hwnd: HWND) -> bool {
    type IsWindowArrangedFn = unsafe extern "system" fn(HWND) -> BOOL;
    static FUNCTION: OnceLock<Option<IsWindowArrangedFn>> = OnceLock::new();
    let function = FUNCTION.get_or_init(|| {
        // SAFETY: user32 is always loaded in a GUI process; the transmute matches the
        // documented signature of IsWindowArranged.
        unsafe {
            let user32 = GetModuleHandleW(w!("user32.dll")).ok()?;
            let proc = GetProcAddress(user32, windows::core::s!("IsWindowArranged"))?;
            Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, IsWindowArrangedFn>(
                proc,
            ))
        }
    });
    // SAFETY: calls the resolved IsWindowArranged with a window handle.
    function.is_some_and(|f| unsafe { f(hwnd) }.as_bool())
}

fn dpi_awareness_of(context: DPI_AWARENESS_CONTEXT) -> DpiAwareness {
    // SAFETY: pure queries on a context value returned by Windows.
    unsafe {
        if AreDpiAwarenessContextsEqual(context, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            .as_bool()
        {
            return DpiAwareness::PerMonitorV2;
        }
        match GetAwarenessFromDpiAwarenessContext(context) {
            DPI_AWARENESS_UNAWARE => DpiAwareness::Unaware,
            DPI_AWARENESS_SYSTEM_AWARE => DpiAwareness::System,
            DPI_AWARENESS_PER_MONITOR_AWARE => DpiAwareness::PerMonitor,
            _ => DpiAwareness::Unknown,
        }
    }
}

fn monitor_device(monitor: HMONITOR) -> Option<(String, MONITORINFOEXW)> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: MONITORINFOEXW starts with MONITORINFO and cbSize says which one it is.
    let ok = unsafe {
        GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>())
    };
    if !ok.as_bool() {
        return None;
    }
    let len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
    Some((wide_to_string(&info.szDevice, len), info))
}

/// The real window manager. Stateless apart from the cached own pid, so a fresh one can be
/// created anywhere (the emergency path does exactly that).
pub struct Win32WindowManager {
    own_pid: u32,
}

impl Default for Win32WindowManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Win32WindowManager {
    pub fn new() -> Self {
        // SAFETY: no preconditions.
        Self { own_pid: unsafe { GetCurrentProcessId() } }
    }

    fn check_window(&self, hwnd: HWND) -> Result<(), AppError> {
        if self.is_window(from_hwnd(hwnd)) {
            Ok(())
        } else {
            Err(AppError::WindowGone)
        }
    }

    fn read_long(&self, hwnd: HWND) -> Result<(u32, u32), AppError> {
        self.check_window(hwnd)?;
        // SAFETY: reads two window longs from a window that exists (checked above).
        let (style, ex_style) =
            unsafe { (GetWindowLongPtrW(hwnd, GWL_STYLE), GetWindowLongPtrW(hwnd, GWL_EXSTYLE)) };
        Ok((style as u32, ex_style as u32))
    }

    fn find_trays(&self, class: PCWSTR, expected: &str, trays: &mut Vec<TrayWindow>) {
        let mut previous: Option<HWND> = None;
        for _ in 0..16 {
            // SAFETY: enumerates top-level windows of a class; ends with an error.
            let Ok(hwnd) = (unsafe { FindWindowExW(None, previous, class, PCWSTR::null()) }) else {
                break;
            };
            previous = Some(hwnd);
            // Win11 21H2's FindWindow could return other classes; YASB/Zebar reuse the class.
            if class_name(hwnd) != expected {
                continue;
            }
            let pid = self.window_pid(from_hwnd(hwnd)).unwrap_or(0);
            let exe = process_exe_name(pid);
            if !exe.eq_ignore_ascii_case("explorer.exe") {
                continue;
            }
            // SAFETY: visibility query on an existing window.
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            trays.push(TrayWindow {
                hwnd: from_hwnd(hwnd),
                class: expected.to_string(),
                exe,
                visible,
            });
        }
    }
}

impl WindowManager for Win32WindowManager {
    fn own_pid(&self) -> u32 {
        self.own_pid
    }

    fn foreground(&self) -> Option<isize> {
        // SAFETY: no preconditions.
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_invalid()).then(|| from_hwnd(hwnd))
    }

    fn top_level_windows(&self) -> Vec<isize> {
        let mut windows = Vec::new();
        // SAFETY: walks the top-level z-order; each step ends with an error at the bottom.
        let mut next = unsafe { GetTopWindow(None) }.ok();
        while let Some(hwnd) = next {
            windows.push(from_hwnd(hwnd));
            if windows.len() >= 4096 {
                break;
            }
            // SAFETY: as above.
            next = unsafe { GetWindow(hwnd, GW_HWNDNEXT) }.ok();
        }
        windows
    }

    fn info(&self, raw: isize) -> Result<WindowInfo, AppError> {
        let hwnd = to_hwnd(raw);
        self.check_window(hwnd)?;
        let mut pid = 0u32;
        // SAFETY: valid out-pointer; read-only queries on an existing window.
        let (tid, root, visible, iconic, zoomed, dpi, context, monitor) = unsafe {
            let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            (
                tid,
                GetAncestor(hwnd, GA_ROOT),
                IsWindowVisible(hwnd).as_bool(),
                IsIconic(hwnd).as_bool(),
                IsZoomed(hwnd).as_bool(),
                GetDpiForWindow(hwnd),
                GetWindowDpiAwarenessContext(hwnd),
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
            )
        };
        if tid == 0 {
            return Err(AppError::WindowGone);
        }
        let (_, ex_style) = self.read_long(hwnd)?;
        Ok(WindowInfo {
            hwnd: raw,
            root: from_hwnd(root),
            pid,
            tid,
            exe: process_exe_name(pid),
            title: window_title(hwnd),
            class: class_name(hwnd),
            visible,
            cloaked: is_cloaked(hwnd),
            iconic,
            zoomed,
            arranged: is_arranged(hwnd),
            ex_style,
            dpi,
            dpi_awareness: dpi_awareness_of(context),
            monitor: monitor_device(monitor).map(|(device, _)| device).unwrap_or_default(),
        })
    }

    fn integrity_level(&self, pid: u32) -> Option<u32> {
        let process = open_process_query(pid)?;
        token_integrity_level(process.0)
    }

    fn probe_access(&self, raw: isize) -> Result<(), AppError> {
        let flags = SWP_NOMOVE
            | SWP_NOSIZE
            | SWP_NOZORDER
            | SWP_NOACTIVATE
            | SWP_NOOWNERZORDER
            | SWP_NOSENDCHANGING;
        // SAFETY: a no-op SetWindowPos; UIPI makes it fail with ERROR_ACCESS_DENIED.
        unsafe {
            SetLastError(ERROR_SUCCESS);
            SetWindowPos(to_hwnd(raw), None, 0, 0, 0, 0, flags)
        }
        .map_err(|e| api_error("SetWindowPos", &e))
    }

    fn styles(&self, raw: isize) -> Result<(u32, u32), AppError> {
        self.read_long(to_hwnd(raw))
    }

    fn set_styles(&self, raw: isize, style: u32, ex_style: u32) -> Result<(u32, u32), AppError> {
        let hwnd = to_hwnd(raw);
        self.check_window(hwnd)?;
        for (index, value, api) in [
            (GWL_STYLE, style, "SetWindowLongPtrW(GWL_STYLE)"),
            (GWL_EXSTYLE, ex_style, "SetWindowLongPtrW(GWL_EXSTYLE)"),
        ] {
            // SAFETY: writes a style word to an existing window. A zero return is only an
            // error when the last-error value was set (0 can be a legitimate old value).
            let previous = unsafe {
                SetLastError(ERROR_SUCCESS);
                SetWindowLongPtrW(hwnd, index, value as i32 as isize)
            };
            let error = last_error();
            if previous == 0 && error != ERROR_SUCCESS {
                if error == ERROR_ACCESS_DENIED {
                    return Err(AppError::win32(api, ERROR_ACCESS_DENIED.0));
                }
                log::warn!("{api} failed with {}", error.0);
            }
        }
        self.read_long(hwnd)
    }

    fn placement(&self, raw: isize) -> Result<Placement, AppError> {
        let mut wp =
            WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        // SAFETY: valid WINDOWPLACEMENT with its length set.
        unsafe { GetWindowPlacement(to_hwnd(raw), &mut wp) }
            .map_err(|e| api_error("GetWindowPlacement", &e))?;
        Ok(Placement {
            show_cmd: wp.showCmd,
            flags: wp.flags.0,
            min_pos: Point { x: wp.ptMinPosition.x, y: wp.ptMinPosition.y },
            max_pos: Point { x: wp.ptMaxPosition.x, y: wp.ptMaxPosition.y },
            normal: rect_from(wp.rcNormalPosition),
        })
    }

    fn set_placement(&self, raw: isize, p: &Placement) -> Result<(), AppError> {
        let wp = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            flags: WINDOWPLACEMENT_FLAGS(p.flags),
            showCmd: p.show_cmd,
            ptMinPosition: POINT { x: p.min_pos.x, y: p.min_pos.y },
            ptMaxPosition: POINT { x: p.max_pos.x, y: p.max_pos.y },
            rcNormalPosition: rect_to(p.normal),
        };
        // SAFETY: valid WINDOWPLACEMENT with its length set.
        unsafe { SetWindowPlacement(to_hwnd(raw), &wp) }
            .map_err(|e| api_error("SetWindowPlacement", &e))
    }

    fn rect(&self, raw: isize) -> Result<Rect, AppError> {
        let mut r = RECT::default();
        // SAFETY: valid out-pointer.
        unsafe { GetWindowRect(to_hwnd(raw), &mut r) }
            .map_err(|e| api_error("GetWindowRect", &e))?;
        Ok(rect_from(r))
    }

    fn set_pos(&self, raw: isize, rect: Rect, z: ZOrder, async_pos: bool) -> Result<(), AppError> {
        let hwnd = to_hwnd(raw);
        let mut flags: SET_WINDOW_POS_FLAGS = SWP_FRAMECHANGED
            | SWP_NOACTIVATE
            | SWP_NOOWNERZORDER
            | SWP_NOSENDCHANGING
            | SWP_NOCOPYBITS;
        let insert_after = match z {
            ZOrder::Keep => {
                flags |= SWP_NOZORDER;
                None
            }
            ZOrder::TopMost => Some(HWND_TOPMOST),
            ZOrder::NoTopMost => Some(HWND_NOTOPMOST),
        };
        if async_pos {
            // A hung game can't block the engine on a cross-process SetWindowPos.
            flags |= SWP_ASYNCWINDOWPOS;
        }
        // SAFETY: positions an existing window; errors are returned.
        unsafe { SetWindowPos(hwnd, insert_after, rect.x, rect.y, rect.w, rect.h, flags) }
            .map_err(|e| api_error("SetWindowPos", &e))?;
        // SAFETY: repaint request for the same window; failure is harmless.
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
        Ok(())
    }

    fn show(&self, raw: isize, cmd: ShowCmd) -> Result<(), AppError> {
        let hwnd = to_hwnd(raw);
        self.check_window(hwnd)?;
        // SAFETY: ShowWindow(Async) on an existing window; the return value is the previous
        // visibility, not success, so it is ignored.
        unsafe {
            let _ = match cmd {
                ShowCmd::Restore => ShowWindow(hwnd, SW_RESTORE),
                ShowCmd::ShowMinNoActive => ShowWindow(hwnd, SW_SHOWMINNOACTIVE),
                ShowCmd::HideAsync => ShowWindowAsync(hwnd, SW_HIDE),
                ShowCmd::ShowNaAsync => ShowWindowAsync(hwnd, SW_SHOWNA),
            };
        }
        Ok(())
    }

    fn set_foreground(&self, raw: isize) -> bool {
        // SAFETY: no preconditions; Windows may refuse.
        let ok = unsafe { SetForegroundWindow(to_hwnd(raw)) }.as_bool();
        if !ok {
            log::info!("SetForegroundWindow({raw:#x}) refused");
        }
        ok
    }

    fn dwm_square(&self, raw: isize, on: bool) {
        let hwnd = to_hwnd(raw);
        let corner = if on { DWMWCP_DONOTROUND } else { DWMWCP_DEFAULT };
        let border = if on { DWM_COLOR_NONE } else { DWM_COLOR_DEFAULT };
        // SAFETY: 4-byte attribute values from the stack. Errors (Windows 10) are ignored.
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corner as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
                size_of::<i32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                (&border as *const u32).cast(),
                size_of::<u32>() as u32,
            );
        }
    }

    fn is_window(&self, raw: isize) -> bool {
        // SAFETY: no preconditions.
        unsafe { IsWindow(Some(to_hwnd(raw))) }.as_bool()
    }

    fn left_button_down(&self) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        // SAFETY: only reads key state.
        let state = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) };
        state < 0
    }

    fn is_visible(&self, raw: isize) -> bool {
        let hwnd = to_hwnd(raw);
        // SAFETY: no preconditions.
        unsafe { IsWindowVisible(hwnd) }.as_bool() && !is_cloaked(hwnd)
    }

    fn is_iconic(&self, raw: isize) -> bool {
        // SAFETY: no preconditions.
        unsafe { IsIconic(to_hwnd(raw)) }.as_bool()
    }

    fn is_zoomed(&self, raw: isize) -> bool {
        // SAFETY: no preconditions.
        unsafe { IsZoomed(to_hwnd(raw)) }.as_bool()
    }

    fn window_pid(&self, raw: isize) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: valid out-pointer.
        let tid = unsafe { GetWindowThreadProcessId(to_hwnd(raw), Some(&mut pid)) };
        (tid != 0).then_some(pid)
    }

    fn tray_windows(&self) -> Vec<TrayWindow> {
        let mut trays = Vec::new();
        self.find_trays(w!("Shell_TrayWnd"), PRIMARY_TRAY_CLASS, &mut trays);
        self.find_trays(w!("Shell_SecondaryTrayWnd"), SECONDARY_TRAY_CLASS, &mut trays);
        trays
    }

    fn open_process(&self, pid: u32) -> Result<ProcessHandle, AppError> {
        // SAFETY: the handle is returned to the engine, which closes it via close_process.
        let handle = unsafe {
            OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
        }
        .map_err(|e| api_error("OpenProcess", &e))?;
        Ok(ProcessHandle { raw: handle.0 as isize, pid, created: process_creation_time(handle) })
    }

    fn process_exited(&self, handle: &ProcessHandle) -> bool {
        // SAFETY: a zero-timeout wait on a handle this module opened.
        unsafe { WaitForSingleObject(HANDLE(handle.raw as *mut c_void), 0) == WAIT_OBJECT_0 }
    }

    fn close_process(&self, handle: &ProcessHandle) {
        // SAFETY: closes a handle this module opened; the engine calls this once per handle.
        unsafe {
            let _ = CloseHandle(HANDLE(handle.raw as *mut c_void));
        }
    }

    fn process_alive(&self, pid: u32, created: u64) -> bool {
        // SAFETY: the handle is wrapped and closed on drop.
        let Ok(handle) = (unsafe {
            OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
        }) else {
            return false;
        };
        let handle = OwnedHandle(handle);
        // SAFETY: zero-timeout wait on an open handle.
        let running = unsafe { WaitForSingleObject(handle.0, 0) } != WAIT_OBJECT_0;
        running && (created == 0 || process_creation_time(handle.0) == created)
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        unsafe extern "system" fn collect(
            monitor: HMONITOR,
            _dc: HDC,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            // SAFETY: `data` is the address of the Vec passed to EnumDisplayMonitors below,
            // alive for the whole enumeration.
            let handles = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
            handles.push(monitor);
            BOOL(1)
        }
        let mut handles: Vec<HMONITOR> = Vec::new();
        // SAFETY: the callback only pushes into `handles`, which outlives the call.
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(collect),
                LPARAM(&mut handles as *mut _ as isize),
            );
        }
        let mut monitors: Vec<MonitorInfo> = handles
            .into_iter()
            .filter_map(|handle| {
                let (device, info) = monitor_device(handle)?;
                let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
                // SAFETY: two valid out-pointers.
                let _ =
                    unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
                Some(MonitorInfo {
                    name: crate::model::display_name(&device),
                    device,
                    rect: rect_from(info.monitorInfo.rcMonitor),
                    work: rect_from(info.monitorInfo.rcWork),
                    dpi: dpi_x,
                    primary: info.monitorInfo.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY
                })
            })
            .collect();
        monitors.sort_by_key(|m| (m.rect.x, m.rect.y));
        monitors
    }

    fn settle(&self) {
        std::thread::sleep(Duration::from_millis(40));
    }
}

// ---------------------------------------------------------------------------------------------
// Process-wide helpers
// ---------------------------------------------------------------------------------------------

static EARLY_DPI_RESULT: OnceLock<String> = OnceLock::new();

/// Must be the first statement of `main`: tao sets Per-Monitor-V2 only inside
/// `EventLoop::new`, and geometry read before that is DPI-virtualized.
pub fn set_dpi_awareness_early() {
    // SAFETY: process-wide setting, called once before any window exists.
    let result =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let text = match result {
        Ok(()) => "set to PerMonitorV2 in main".to_string(),
        Err(e) => format!("SetProcessDpiAwarenessContext failed ({})", error_code(&e)),
    };
    let _ = EARLY_DPI_RESULT.set(text);
}

/// The thread's DPI awareness plus what `main` did, for the startup log and diagnostics.
pub fn dpi_context_description() -> String {
    // SAFETY: reads the calling thread's awareness context.
    let context = unsafe { GetThreadDpiAwarenessContext() };
    // SAFETY: compares two context values.
    let legacy_pm =
        unsafe { AreDpiAwarenessContextsEqual(context, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE) }
            .as_bool();
    let awareness = if legacy_pm { DpiAwareness::PerMonitor } else { dpi_awareness_of(context) };
    let early = EARLY_DPI_RESULT.get().map_or("not set in main", String::as_str);
    format!("{awareness:?} ({early})")
}

/// True when this process runs elevated (`TokenElevation`).
pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle needs no closing; the token handle is wrapped.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let token = OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut needed = 0u32;
    // SAFETY: the out-buffer is a TOKEN_ELEVATION of the stated size.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut needed,
        )
    };
    ok.is_ok() && elevation.TokenIsElevated != 0
}

fn read_registry_string(
    root: windows::Win32::System::Registry::HKEY,
    key: &HSTRING,
    value: &HSTRING,
) -> Option<String> {
    let mut buffer = [0u16; 512];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer and its byte size describe valid writable memory.
    let status = unsafe {
        RegGetValueW(
            root,
            key,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(wide_to_string(&buffer, len))
}

fn read_registry_dword(
    root: windows::Win32::System::Registry::HKEY,
    key: &HSTRING,
    value: &HSTRING,
) -> Option<u32> {
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: a 4-byte out-buffer.
    let status = unsafe {
        RegGetValueW(
            root,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

/// Windows version for logs and bug reports, e.g. "25H2 (26200.9457)".
pub fn os_build() -> String {
    let key = HSTRING::from(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let build = read_registry_string(HKEY_LOCAL_MACHINE, &key, &HSTRING::from("CurrentBuild"))
        .unwrap_or_else(|| "?".into());
    let ubr = read_registry_dword(HKEY_LOCAL_MACHINE, &key, &HSTRING::from("UBR"));
    let display = read_registry_string(HKEY_LOCAL_MACHINE, &key, &HSTRING::from("DisplayVersion"));
    let number = match ubr {
        Some(ubr) => format!("{build}.{ubr}"),
        None => build,
    };
    match display {
        Some(display) => format!("{display} ({number})"),
        None => number,
    }
}

/// Starts this exe again elevated ("runas"). `ERROR_CANCELLED` (1223) when the user declines.
pub fn relaunch_elevated() -> Result<(), AppError> {
    let exe = std::env::current_exe().map_err(|e| AppError::system(e.to_string()))?;
    let file = HSTRING::from(exe.as_os_str());
    // SAFETY: COM init for ShellExecuteEx on this thread (S_FALSE when already initialized).
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to outlive the call.
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|e| AppError::win32("ShellExecuteExW", error_code(&e)))
}

/// Opens a URL or folder with the shell's default handler.
pub fn shell_open(target: &str) {
    let target = HSTRING::from(target);
    // SAFETY: plain ShellExecuteW with static and owned strings alive for the call.
    let result = unsafe {
        ShellExecuteW(None, w!("open"), &target, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
    };
    if result.0 as isize <= 32 {
        log::warn!("ShellExecuteW({target}) failed: {}", result.0 as isize);
    }
}

/// Warning beep for a failed in-game fit (BorderFit never steals focus from a game).
pub fn beep() {
    // SAFETY: no preconditions.
    let _ = unsafe { MessageBeep(MB_ICONWARNING) };
}

// ---------------------------------------------------------------------------------------------
// Single-instance event (the elevated-first case the single-instance plugin can't see)
// ---------------------------------------------------------------------------------------------

/// `Local\`: one per logon session, like the single-instance plugin's own mutex. An elevated and
/// a normal BorderFit in the same session share it; other users' sessions don't see it.
const INSTANCE_EVENT: &str = r"Local\BorderFit";
/// Everyone may SYNCHRONIZE; low mandatory label with no-write-up so a Medium-IL instance can
/// open the event of an elevated one.
const INSTANCE_SDDL: &str = "D:(A;;0x100000;;;WD)S:(ML;;NW;;;LW)";

static INSTANCE_HANDLE: Mutex<Option<isize>> = Mutex::new(None);

/// True when another BorderFit (possibly elevated) holds the instance event.
pub fn other_instance_running() -> bool {
    let name = HSTRING::from(INSTANCE_EVENT);
    // SAFETY: opens a named event; the handle is closed right away.
    match unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, &name) } {
        Ok(handle) => {
            drop(OwnedHandle(handle));
            true
        }
        Err(e) => error_code(&e) != ERROR_FILE_NOT_FOUND.0,
    }
}

/// Creates the instance event for this process's lifetime.
pub fn create_instance_event() {
    let name = HSTRING::from(INSTANCE_EVENT);
    let sddl = HSTRING::from(INSTANCE_SDDL);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: converts a constant SDDL string; the descriptor is freed after CreateEventW.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &sddl,
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    };
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: BOOL(0),
    };
    let attributes_ptr = converted.is_ok().then_some(&attributes as *const SECURITY_ATTRIBUTES);
    // SAFETY: the attributes (when present) point to a valid descriptor.
    let created = unsafe { CreateEventW(attributes_ptr, true, false, &name) };
    if converted.is_ok() {
        // SAFETY: frees the LocalAlloc'd descriptor exactly once.
        unsafe {
            let _ = windows::Win32::Foundation::LocalFree(Some(
                windows::Win32::Foundation::HLOCAL(descriptor.0),
            ));
        }
    }
    match created {
        Ok(handle) => {
            *INSTANCE_HANDLE.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(handle.0 as isize);
        }
        Err(e) => log::warn!("instance event: CreateEventW failed ({})", error_code(&e)),
    }
}

/// Closes the instance event (before an elevated relaunch, so the new instance starts clean).
pub fn close_instance_event() {
    if let Some(raw) = INSTANCE_HANDLE.lock().unwrap_or_else(PoisonError::into_inner).take() {
        drop(OwnedHandle(HANDLE(raw as *mut c_void)));
    }
}

// ---------------------------------------------------------------------------------------------
// Launch at startup (HKCU Run value, quoted path)
// ---------------------------------------------------------------------------------------------

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "BorderFit";

/// The Run value for an exe path: quoted path plus `--minimized`.
pub fn autostart_command(exe: &std::path::Path) -> String {
    format!("\"{}\" --minimized", exe.display())
}

pub fn set_autostart(enabled: bool) -> Result<(), AppError> {
    let key = HSTRING::from(RUN_KEY);
    let value = HSTRING::from(RUN_VALUE);
    if !enabled {
        // SAFETY: deletes one value under HKCU.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &value) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return Err(AppError::win32("RegDeleteKeyValueW", status.0));
        }
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| AppError::system(e.to_string()))?;
    let command: Vec<u16> = autostart_command(&exe).encode_utf16().chain(Some(0)).collect();
    // SAFETY: REG_SZ data with its byte length, including the terminating null.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &key,
            &value,
            REG_SZ.0,
            Some(command.as_ptr().cast()),
            (command.len() * 2) as u32,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(AppError::win32("RegSetKeyValueW", status.0));
    }
    Ok(())
}

/// The current Run value, if any.
pub fn autostart_value() -> Option<String> {
    read_registry_string(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &HSTRING::from(RUN_VALUE))
}

// ---------------------------------------------------------------------------------------------
// Subclass of the main window: TaskbarCreated and session end
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellMessage {
    /// Explorer (re)created the taskbar (restart, primary DPI change).
    TaskbarCreated,
    /// Logoff/shutdown was requested: restore now (it may still be cancelled).
    QueryEndSession,
    /// The session is ending: the process will be killed next.
    EndSession,
}

type ShellHandler = Box<dyn Fn(ShellMessage) + Send + Sync>;
static SHELL_HANDLER: OnceLock<ShellHandler> = OnceLock::new();
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
const SUBCLASS_ID: usize = 0xB0F1;

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Relaxed);
    if let Some(handler) = SHELL_HANDLER.get() {
        if taskbar_created != 0 && msg == taskbar_created {
            handler(ShellMessage::TaskbarCreated);
        } else if msg == WM_QUERYENDSESSION {
            handler(ShellMessage::QueryEndSession);
        } else if msg == WM_ENDSESSION && wparam.0 != 0 {
            handler(ShellMessage::EndSession);
        }
    }
    // SAFETY: forwards to the next window procedure in the subclass chain.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Subclasses BorderFit's (hidden, unowned, top-level) main window so it hears the
/// `TaskbarCreated` broadcast (also through UIPI when elevated) and session-end messages.
/// Must run on the thread that owns the window.
pub fn install_subclass(hwnd: isize, handler: impl Fn(ShellMessage) + Send + Sync + 'static) {
    let _ = SHELL_HANDLER.set(Box::new(handler));
    // SAFETY: registers a message name and adjusts this window's UIPI filter.
    unsafe {
        let message = RegisterWindowMessageW(w!("TaskbarCreated"));
        TASKBAR_CREATED.store(message, Ordering::Relaxed);
        if let Err(e) = ChangeWindowMessageFilterEx(to_hwnd(hwnd), message, MSGFLT_ALLOW, None) {
            log::warn!("ChangeWindowMessageFilterEx failed: {}", error_code(&e));
        }
    }
    // SAFETY: the subclass proc is a static function; this runs on the window's thread.
    let ok = unsafe { SetWindowSubclass(to_hwnd(hwnd), Some(subclass_proc), SUBCLASS_ID, 0) };
    if !ok.as_bool() {
        log::error!("SetWindowSubclass failed");
    }
}
