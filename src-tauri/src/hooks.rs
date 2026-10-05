//! The WinEvent hook thread: pid-scoped EVENT_OBJECT_DESTROY/HIDE/CLOAKED/LOCATIONCHANGE for the
//! tracked window, so a closed or hidden game is noticed at once instead of on the next tick, and
//! a window that moves out of its region (a fullscreen browser after another window is
//! activated) is put back at once.
//!
//! WinEvent hooks need a thread that pumps messages, and `UnhookWinEvent` must run on the
//! thread that installed the hook, so this thread owns both and is driven by thread messages:
//! WM_APP+1 track, WM_APP+2 untrack, WM_QUIT stop. The callback only sends on a channel.

#[cfg(not(windows))]
use crate::engine::Msg;
#[cfg(not(windows))]
use std::sync::mpsc::Sender;

#[cfg(windows)]
pub use platform::HookThread;

#[cfg(windows)]
mod platform {
    use crate::engine::{HookControl, Msg};
    use crate::win32::{
        EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE,
    };
    use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
    use std::sync::mpsc::{sync_channel, Sender};
    use std::sync::OnceLock;
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, CHILDID_SELF, MSG,
        OBJID_WINDOW, PM_NOREMOVE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_QUIT,
    };

    const WM_TRACK: u32 = WM_APP + 1;
    const WM_UNTRACK: u32 = WM_APP + 2;

    /// What the callback needs: where to send, and which window/generation is tracked.
    struct Context {
        tx: Sender<Msg>,
        hwnd: AtomicIsize,
        gen: AtomicU64,
    }

    static CONTEXT: OnceLock<Context> = OnceLock::new();

    /// Handle to the hook thread; also the engine's `HookControl`.
    pub struct HookThread {
        tid: u32,
        join: Option<JoinHandle<()>>,
    }

    impl HookThread {
        /// Starts the thread and waits until its message queue exists.
        pub fn spawn(tx: Sender<Msg>) -> std::io::Result<Self> {
            let _ = CONTEXT.set(Context { tx, hwnd: AtomicIsize::new(0), gen: AtomicU64::new(0) });
            let (ready_tx, ready_rx) = sync_channel(1);
            let join =
                std::thread::Builder::new().name("winevent-hooks".into()).spawn(move || {
                    let mut msg = MSG::default();
                    // SAFETY: creates this thread's message queue before anyone posts to it.
                    unsafe {
                        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                        let _ = ready_tx.send(GetCurrentThreadId());
                    }
                    message_loop();
                })?;
            let tid = ready_rx.recv().unwrap_or(0);
            Ok(Self { tid, join: Some(join) })
        }

        /// A `HookControl` that posts to this thread (cheap to create, `Send`).
        pub fn control(&self) -> HookPoster {
            HookPoster { tid: self.tid }
        }

        /// Stops the thread (unhooking on it) and waits for it.
        pub fn stop(&mut self) {
            post(self.tid, WM_QUIT, 0, 0);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    /// Posts track/untrack requests to the hook thread.
    pub struct HookPoster {
        tid: u32,
    }

    impl HookControl for HookPoster {
        fn track(&self, pid: u32, hwnd: isize, gen: u64) {
            if let Some(context) = CONTEXT.get() {
                context.hwnd.store(hwnd, Ordering::SeqCst);
                context.gen.store(gen, Ordering::SeqCst);
            }
            post(self.tid, WM_TRACK, pid as usize, 0);
        }

        fn untrack(&self) {
            if let Some(context) = CONTEXT.get() {
                context.hwnd.store(0, Ordering::SeqCst);
            }
            post(self.tid, WM_UNTRACK, 0, 0);
        }
    }

    fn post(tid: u32, msg: u32, wparam: usize, lparam: isize) {
        if tid == 0 {
            return;
        }
        // SAFETY: posts a plain thread message; no pointers are passed.
        if let Err(e) = unsafe { PostThreadMessageW(tid, msg, WPARAM(wparam), LPARAM(lparam)) } {
            log::warn!("hooks: PostThreadMessageW({msg:#x}) failed: {e}");
        }
    }

    fn message_loop() {
        let mut hooks: Vec<HWINEVENTHOOK> = Vec::new();
        let mut msg = MSG::default();
        // SAFETY: standard message loop on this thread; hooks are installed and removed here.
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                match msg.message {
                    WM_TRACK => {
                        unhook_all(&mut hooks);
                        hooks = install(msg.wParam.0 as u32);
                    }
                    WM_UNTRACK => unhook_all(&mut hooks),
                    _ => {
                        DispatchMessageW(&msg);
                    }
                }
            }
            unhook_all(&mut hooks);
        }
        log::info!("hooks: thread stopped");
    }

    /// Out-of-context hooks scoped to one process: DESTROY..HIDE, LOCATIONCHANGE and CLOAKED.
    /// The callback drops every event that isn't about the tracked window itself (carets,
    /// cursors and child windows of the same process included).
    fn install(pid: u32) -> Vec<HWINEVENTHOOK> {
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let ranges = [
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CLOAKED),
        ];
        ranges
            .iter()
            .filter_map(|&(min, max)| {
                // SAFETY: the callback is a static function; the hook belongs to this thread.
                let hook =
                    unsafe { SetWinEventHook(min, max, None, Some(callback), pid, 0, flags) };
                if hook.is_invalid() {
                    log::warn!("hooks: SetWinEventHook({min:#x}) failed for pid {pid}");
                    None
                } else {
                    Some(hook)
                }
            })
            .collect()
    }

    fn unhook_all(hooks: &mut Vec<HWINEVENTHOOK>) {
        for hook in hooks.drain(..) {
            // SAFETY: each hook was installed on this thread.
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
    }

    /// Runs on the hook thread for every event of the tracked process. Only filters and sends:
    /// USER frees the event's memory when this returns.
    unsafe extern "system" fn callback(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_object: i32,
        id_child: i32,
        _thread: u32,
        _time: u32,
    ) {
        let Some(context) = CONTEXT.get() else { return };
        let tracked = context.hwnd.load(Ordering::SeqCst);
        if id_object != OBJID_WINDOW.0
            || id_child != CHILDID_SELF as i32
            || hwnd.0 as isize != tracked
        {
            return;
        }
        let gen = context.gen.load(Ordering::SeqCst);
        let _ = context.tx.send(Msg::WinEvent { gen, event, hwnd: tracked });
    }
}

/// Stand-in on non-Windows builds: no hooks, the engine's tick covers everything.
#[cfg(not(windows))]
pub struct HookThread;

#[cfg(not(windows))]
impl HookThread {
    pub fn spawn(_tx: Sender<Msg>) -> std::io::Result<Self> {
        Ok(Self)
    }

    pub fn control(&self) -> crate::engine::NoHooks {
        crate::engine::NoHooks
    }

    pub fn stop(&mut self) {}
}
