#[cfg(windows)]
mod plugin {
    use std::{
        cmp::min,
        collections::HashMap,
        ffi::c_void,
        panic::catch_unwind,
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc, Mutex, OnceLock,
        },
        thread,
        time::{Duration, Instant},
    };

    use aviutl2::{module::ScriptModuleFunctions, AnyResult};
    use encoding_rs::SHIFT_JIS;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{BOOL, HWND, LPARAM, WPARAM},
            System::{
                Diagnostics::Debug::OutputDebugStringW,
                Threading::{GetCurrentProcessId, GetCurrentThreadId},
            },
            UI::{
                Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
                WindowsAndMessaging::{
                    DispatchMessageW, EnumChildWindows, GetAncestor, GetClassNameW, GetMessageW,
                    GetWindowLongPtrW, IsWindow, IsWindowVisible, PeekMessageW, PostThreadMessageW,
                    SendMessageA, TranslateMessage, CB_GETCOUNT, CB_GETLBTEXT, CB_GETLBTEXTLEN,
                    EVENT_OBJECT_CREATE, EVENT_OBJECT_FOCUS, EVENT_OBJECT_SHOW, GA_ROOT, GWL_STYLE,
                    MSG, OBJID_CLIENT, OBJID_WINDOW, PM_NOREMOVE, WINEVENT_OUTOFCONTEXT, WM_APP,
                    WM_QUIT, WS_DISABLED,
                },
            },
        },
    };

    const MAX_ITEMS_TO_EXTRACT: usize = 5;
    const STRONG_CANDIDATE_SCORE: i32 = 8;
    const PROBE_MIN_INTERVAL_MS: u64 = 150;
    const PROBE_RETRY_DELAYS_MS: [u64; 3] = [80, 220, 500];
    const WM_SPIKE_PROBE: u32 = WM_APP + 0x51;
    const COMBO_STYLE_TYPE_MASK: u32 = 0x0003;
    const COMBO_STYLE_DROPDOWNLIST: u32 = 0x0003;

    static PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    static LAST_PROBE_BY_ROOT: OnceLock<Mutex<HashMap<isize, Instant>>> = OnceLock::new();

    #[aviutl2::plugin(ScriptModule)]
    struct ComboProbeModule {
        hook_thread: Option<HookThreadController>,
    }

    // SAFETY: The module contains only thread-safe fields (u32 and JoinHandle).
    unsafe impl Send for ComboProbeModule {}
    unsafe impl Sync for ComboProbeModule {}

    impl aviutl2::module::ScriptModule for ComboProbeModule {
        fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
            let process_id = unsafe { GetCurrentProcessId() };
            let hook_thread = HookThreadController::start(process_id);
            Ok(Self { hook_thread })
        }

        fn plugin_info(&self) -> aviutl2::module::ScriptModuleTable {
            aviutl2::module::ScriptModuleTable {
                information: format!(
                    "ComboBox probe module for AviUtl2 (spike v5) / v{version}",
                    version = env!("CARGO_PKG_VERSION"),
                ),
                functions: Self::functions(),
            }
        }
    }

    impl Drop for ComboProbeModule {
        fn drop(&mut self) {
            if let Some(mut hook_thread) = self.hook_thread.take() {
                hook_thread.stop();
            }
        }
    }

    #[aviutl2::module::functions]
    impl ComboProbeModule {
        fn spike_status(&self) -> aviutl2::AnyResult<String> {
            Ok("Combo probe v6 identification spike is active".to_string())
        }
    }

    #[derive(Clone, Debug)]
    struct ComboCandidate {
        hwnd: HWND,
        count: isize,
        score: i32,
        is_dropdownlist: bool,
        visible: bool,
        enabled: bool,
        decode_errors: usize,
        samples: Vec<String>,
    }

    struct ProbeOutcome {
        strong_found: bool,
    }

    impl Default for ProbeOutcome {
        fn default() -> Self {
            Self {
                strong_found: false,
            }
        }
    }

    struct HookThreadController {
        thread_id: u32,
        join_handle: Option<thread::JoinHandle<()>>,
    }

    impl HookThreadController {
        fn start(process_id: u32) -> Option<Self> {
            let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();

            let join_handle = thread::spawn(move || {
                let mut msg = MSG::default();
                // Ensure this thread owns a message queue before installing the hook.
                let _ = unsafe { PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_NOREMOVE) };
                let thread_id = unsafe { GetCurrentThreadId() };

                let events = [EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, EVENT_OBJECT_FOCUS];
                let mut hooks = Vec::new();
                for event in events {
                    let hook = unsafe {
                        SetWinEventHook(
                            event,
                            event,
                            None,
                            Some(win_event_proc),
                            process_id,
                            0,
                            WINEVENT_OUTOFCONTEXT,
                        )
                    };

                    if hook.0.is_null() {
                        debug_log(&format!(
                            "[SpikeTest] SetWinEventHook failed event={}",
                            event_name(event)
                        ));
                    } else {
                        debug_log(&format!(
                            "[SpikeTest] SetWinEventHook installed event={}",
                            event_name(event)
                        ));
                        hooks.push(hook);
                    }
                }

                if hooks.is_empty() {
                    let _ = ready_tx.send(Err(
                        "no WinEvent hooks were installed; hook thread exiting".to_string(),
                    ));
                    return;
                }

                let _ = ready_tx.send(Ok(thread_id));
                debug_log(&format!(
                    "[SpikeTest] hook thread started tid={} hooks={}",
                    thread_id,
                    hooks.len()
                ));

                loop {
                    let result = unsafe { GetMessageW(&mut msg, HWND::default(), 0, 0).0 };
                    if result == -1 {
                        debug_log("[SpikeTest] GetMessageW failed on hook thread");
                        break;
                    }
                    if result == 0 {
                        break;
                    }
                    if msg.message == WM_SPIKE_PROBE {
                        let root = raw_to_hwnd(msg.wParam.0);
                        let attempt = msg.lParam.0 as u32;
                        if catch_unwind(|| unsafe {
                            let _ = run_probe(root, "DELAYED", attempt);
                        })
                        .is_err()
                        {
                            debug_log("[SpikeTest] panic in delayed probe handler");
                        }
                        continue;
                    }

                    unsafe {
                        let _ = TranslateMessage(&msg);
                        let _ = DispatchMessageW(&msg);
                    }
                }

                for hook in hooks {
                    let _ = unsafe { UnhookWinEvent(hook) };
                }
                debug_log("[SpikeTest] hook thread stopped and hooks uninstalled");
            });

            match ready_rx.recv() {
                Ok(Ok(thread_id)) => Some(Self {
                    thread_id,
                    join_handle: Some(join_handle),
                }),
                Ok(Err(message)) => {
                    debug_log(&format!("[SpikeTest] {message}"));
                    let _ = join_handle.join();
                    None
                }
                Err(_) => {
                    debug_log("[SpikeTest] hook thread startup channel closed unexpectedly");
                    let _ = join_handle.join();
                    None
                }
            }
        }

        fn stop(&mut self) {
            let posted =
                unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
                    .is_ok();
            if !posted {
                debug_log("[SpikeTest] failed to post WM_QUIT to hook thread");
            } else {
                debug_log("[SpikeTest] WM_QUIT posted to hook thread");
            }

            if let Some(join_handle) = self.join_handle.take() {
                if join_handle.join().is_err() {
                    debug_log("[SpikeTest] hook thread panicked during join");
                }
            }
        }
    }

    fn event_name(event: u32) -> &'static str {
        match event {
            EVENT_OBJECT_CREATE => "EVENT_OBJECT_CREATE",
            EVENT_OBJECT_SHOW => "EVENT_OBJECT_SHOW",
            EVENT_OBJECT_FOCUS => "EVENT_OBJECT_FOCUS",
            _ => "UNKNOWN_EVENT",
        }
    }

    fn debug_log(msg: &str) {
        let wide: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { OutputDebugStringW(PCWSTR(wide.as_ptr())) };
    }

    unsafe extern "system" fn win_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_object: i32,
        id_child: i32,
        event_thread: u32,
        _event_time: u32,
    ) {
        let result = catch_unwind(|| unsafe {
            if !is_probe_trigger(event, hwnd, id_object, id_child) {
                return;
            }

            let root = resolve_root_window(hwnd);
            if root.0.is_null() || !IsWindow(root).as_bool() {
                return;
            }
            if !should_probe_now(root) {
                return;
            }

            debug_log(&format!(
                "[SpikeTest] Trigger event={} root={:?} hwnd={:?} obj={} child={} event_thread={}",
                event_name(event),
                root,
                hwnd,
                id_object,
                id_child,
                event_thread
            ));

            let outcome = run_probe(root, event_name(event), 0);
            if !outcome.strong_found {
                schedule_probe_retries(root);
            }
        });
        if result.is_err() {
            debug_log("[SpikeTest] panic in WinEventProc callback");
        }
    }

    unsafe extern "system" fn collect_combo_proc(hwnd: HWND, l_param: LPARAM) -> BOOL {
        let result = catch_unwind(|| unsafe {
            let candidates = &mut *(l_param.0 as *mut Vec<ComboCandidate>);
            if let Some(candidate) = collect_combo_candidate(hwnd) {
                candidates.push(candidate);
            }
        });

        if result.is_err() {
            debug_log("[SpikeTest] panic in collect_combo_proc callback");
        }

        BOOL(1)
    }

    fn probe_history() -> &'static Mutex<HashMap<isize, Instant>> {
        LAST_PROBE_BY_ROOT.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn should_probe_now(root: HWND) -> bool {
        let now = Instant::now();
        let mut history = match probe_history().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let key = root.0 as isize;
        if let Some(last) = history.get(&key) {
            if now.duration_since(*last) < Duration::from_millis(PROBE_MIN_INTERVAL_MS) {
                return false;
            }
        }
        history.insert(key, now);
        true
    }

    fn is_probe_trigger(event: u32, hwnd: HWND, id_object: i32, id_child: i32) -> bool {
        if hwnd.0.is_null() {
            return false;
        }

        match event {
            EVENT_OBJECT_CREATE | EVENT_OBJECT_SHOW => id_object == OBJID_WINDOW.0 && id_child == 0,
            EVENT_OBJECT_FOCUS => {
                id_object == OBJID_WINDOW.0 || id_object == OBJID_CLIENT.0 || id_object == 0
            }
            _ => false,
        }
    }

    unsafe fn resolve_root_window(hwnd: HWND) -> HWND {
        let root = GetAncestor(hwnd, GA_ROOT);
        if root.0.is_null() {
            hwnd
        } else {
            root
        }
    }

    fn schedule_probe_retries(root: HWND) {
        let thread_id = unsafe { GetCurrentThreadId() };
        let raw_hwnd = hwnd_to_raw(root);
        debug_log(&format!(
            "[SpikeTest] Scheduling retries root={:?} delays_ms={:?}",
            root, PROBE_RETRY_DELAYS_MS
        ));

        for (index, delay_ms) in PROBE_RETRY_DELAYS_MS.into_iter().enumerate() {
            let attempt = (index + 1) as isize;
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(delay_ms));
                let posted = unsafe {
                    PostThreadMessageW(thread_id, WM_SPIKE_PROBE, WPARAM(raw_hwnd), LPARAM(attempt))
                }
                .is_ok();
                if !posted {
                    debug_log("[SpikeTest] delayed probe message post failed");
                }
            });
        }
    }

    unsafe fn run_probe(root: HWND, source: &str, attempt: u32) -> ProbeOutcome {
        if !IsWindow(root).as_bool() {
            debug_log(&format!(
                "[SpikeTest] Probe skipped source={} attempt={} root={:?} (invalid window)",
                source, attempt, root
            ));
            return ProbeOutcome::default();
        }

        let mut candidates = Vec::new();
        if let Some(candidate) = collect_combo_candidate(root) {
            candidates.push(candidate);
        }

        let ptr = &mut candidates as *mut Vec<ComboCandidate>;
        let _ = EnumChildWindows(root, Some(collect_combo_proc), LPARAM(ptr as isize));

        candidates.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| b.count.cmp(&a.count))
                .then_with(|| (b.hwnd.0 as usize).cmp(&(a.hwnd.0 as usize)))
        });

        let strong_found = candidates.iter().any(|candidate| {
            candidate.score >= STRONG_CANDIDATE_SCORE
                && candidate.count >= 20
                && candidate.decode_errors == 0
        });
        let probe_id = PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);

        if let Some(best) = candidates.first() {
            debug_log(&format!(
                "[SpikeTest] Probe#{} source={} attempt={} root={:?} combos={} strong={} best_score={} best_hwnd={:?} best_count={} best_samples={:?}",
                probe_id,
                source,
                attempt,
                root,
                candidates.len(),
                strong_found,
                best.score,
                best.hwnd,
                best.count,
                best.samples
            ));

            for candidate in candidates.iter().take(3) {
                debug_log(&format!(
                    "[SpikeTest] Candidate hwnd={:?} score={} count={} dropdownlist={} visible={} enabled={} decode_errors={} samples={:?}",
                    candidate.hwnd,
                    candidate.score,
                    candidate.count,
                    candidate.is_dropdownlist,
                    candidate.visible,
                    candidate.enabled,
                    candidate.decode_errors,
                    candidate.samples
                ));
            }
        } else {
            debug_log(&format!(
                "[SpikeTest] Probe#{} source={} attempt={} root={:?} combos=0 strong=false",
                probe_id, source, attempt, root
            ));
        }

        ProbeOutcome { strong_found }
    }

    unsafe fn collect_combo_candidate(hwnd: HWND) -> Option<ComboCandidate> {
        if !is_combo_box(hwnd) {
            return None;
        }

        let count = SendMessageA(hwnd, CB_GETCOUNT, WPARAM(0), LPARAM(0)).0;
        if count <= 0 {
            return None;
        }

        let mut samples = Vec::new();
        let mut decode_errors = 0usize;
        for index in 0..min(MAX_ITEMS_TO_EXTRACT, count as usize) {
            let text_len = SendMessageA(hwnd, CB_GETLBTEXTLEN, WPARAM(index), LPARAM(0)).0;
            if text_len <= 0 {
                continue;
            }

            let mut raw = vec![0u8; text_len as usize + 1];
            let copied = SendMessageA(
                hwnd,
                CB_GETLBTEXT,
                WPARAM(index),
                LPARAM(raw.as_mut_ptr() as isize),
            )
            .0;
            if copied <= 0 {
                continue;
            }

            let bytes = &raw[..copied as usize];
            let (decoded, _, had_errors) = SHIFT_JIS.decode(bytes);
            if had_errors {
                decode_errors += 1;
                continue;
            }
            let text = decoded.trim();
            if text.is_empty() {
                continue;
            }
            samples.push(text.to_string());
        }

        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let is_dropdownlist = (style & COMBO_STYLE_TYPE_MASK) == COMBO_STYLE_DROPDOWNLIST;
        let visible = IsWindowVisible(hwnd).as_bool();
        let enabled = (style & WS_DISABLED.0 as u32) == 0;
        let score = score_candidate(
            count,
            is_dropdownlist,
            visible,
            enabled,
            samples.len(),
            decode_errors,
        );

        Some(ComboCandidate {
            hwnd,
            count,
            score,
            is_dropdownlist,
            visible,
            enabled,
            decode_errors,
            samples,
        })
    }

    fn score_candidate(
        count: isize,
        is_dropdownlist: bool,
        visible: bool,
        enabled: bool,
        sample_len: usize,
        decode_errors: usize,
    ) -> i32 {
        let mut score = 0;
        if is_dropdownlist {
            score += 3;
        }
        if visible {
            score += 2;
        }
        if enabled {
            score += 1;
        }
        if count >= 10 {
            score += 1;
        }
        if count >= 50 {
            score += 2;
        }
        if sample_len >= 3 {
            score += 1;
        } else if sample_len >= 1 {
            score += 0;
        } else {
            score -= 1;
        }
        if decode_errors == 0 {
            score += 1;
        } else {
            score -= 1;
        }
        score
    }

    fn hwnd_to_raw(hwnd: HWND) -> usize {
        hwnd.0 as usize
    }

    fn raw_to_hwnd(raw: usize) -> HWND {
        HWND(raw as *mut c_void)
    }

    unsafe fn is_combo_box(hwnd: HWND) -> bool {
        let mut class_name = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class_name);
        if len <= 0 {
            return false;
        }
        String::from_utf16_lossy(&class_name[..len as usize]) == "ComboBox"
    }

    aviutl2::register_script_module!(ComboProbeModule);
}

#[cfg(not(windows))]
mod plugin {}
