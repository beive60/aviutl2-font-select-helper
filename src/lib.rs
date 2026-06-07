#[cfg(windows)]
mod plugin {
    use std::{cmp::min, panic::catch_unwind};

    use aviutl2::{module::ScriptModuleFunctions, AnyResult};
    use encoding_rs::SHIFT_JIS;
    use windows::{
        core::PWSTR,
        Win32::{
            Foundation::{BOOL, HWND, LPARAM, LRESULT, WPARAM},
            System::Threading::GetCurrentThreadId,
            UI::WindowsAndMessaging::{
                CallNextHookEx, EnumChildWindows, GetClassNameW, SendMessageA, SetWindowsHookExW,
                UnhookWindowsHookEx, CB_GETCOUNT, CB_GETLBTEXT, CB_GETLBTEXTLEN, CWPRETSTRUCT,
                HHOOK, WH_CALLWNDPROCRET, WM_CREATE, WM_INITDIALOG,
            },
        },
    };

    const MIN_ITEMS_TO_LOG: isize = 10;
    const MAX_ITEMS_TO_EXTRACT: usize = 3;

    #[aviutl2::plugin(ScriptModule)]
    struct ComboProbeModule {
        hook: Option<HHOOK>,
    }

    impl aviutl2::module::ScriptModule for ComboProbeModule {
        fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
            let thread_id = unsafe { GetCurrentThreadId() };
            let hook = unsafe {
                SetWindowsHookExW(WH_CALLWNDPROCRET, Some(call_wnd_ret_proc), None, thread_id)
            };

            if hook.is_invalid() {
                eprintln!("[combo-probe] failed to install WH_CALLWNDPROCRET hook");
                return Ok(Self { hook: None });
            }

            eprintln!("[combo-probe] installed WH_CALLWNDPROCRET hook");
            Ok(Self { hook: Some(hook) })
        }

        fn plugin_info(&self) -> aviutl2::module::ScriptModuleTable {
            aviutl2::module::ScriptModuleTable {
                information: format!(
                    "ComboBox probe module for AviUtl2 (spike) / v{version}",
                    version = env!("CARGO_PKG_VERSION"),
                ),
                functions: Self::functions(),
            }
        }
    }

    impl Drop for ComboProbeModule {
        fn drop(&mut self) {
            if let Some(hook) = self.hook.take() {
                let _ = unsafe { UnhookWindowsHookEx(hook) };
                eprintln!("[combo-probe] uninstalled WH_CALLWNDPROCRET hook");
            }
        }
    }

    #[aviutl2::module::functions]
    impl ComboProbeModule {
        fn spike_status(&self) -> aviutl2::AnyResult<String> {
            Ok("Combo probe hook is active".to_string())
        }
    }

    unsafe extern "system" fn call_wnd_ret_proc(
        n_code: i32,
        w_param: WPARAM,
        l_param: LPARAM,
    ) -> LRESULT {
        let _ = catch_unwind(|| unsafe {
            if n_code >= 0 && l_param.0 != 0 {
                let call_data = &*(l_param.0 as *const CWPRETSTRUCT);
                if call_data.message == WM_INITDIALOG || call_data.message == WM_CREATE {
                    let _ = EnumChildWindows(call_data.hwnd, Some(enum_child_proc), LPARAM(0));
                }
            }
        });

        unsafe { CallNextHookEx(None, n_code, w_param, l_param) }
    }

    unsafe extern "system" fn enum_child_proc(hwnd: HWND, _l_param: LPARAM) -> BOOL {
        let result = catch_unwind(|| unsafe {
            if !is_combo_box(hwnd) {
                return;
            }

            extract_combo_items(hwnd);
        });

        if result.is_err() {
            eprintln!("[combo-probe] panic in EnumChildWindows callback");
        }

        BOOL(1)
    }

    unsafe fn is_combo_box(hwnd: HWND) -> bool {
        let mut class_name = [0u16; 64];
        let len = GetClassNameW(
            hwnd,
            PWSTR(class_name.as_mut_ptr()),
            class_name.len() as i32,
        );
        if len <= 0 {
            return false;
        }

        String::from_utf16_lossy(&class_name[..len as usize]) == "ComboBox"
    }

    unsafe fn extract_combo_items(hwnd: HWND) {
        let count = SendMessageA(hwnd, CB_GETCOUNT, WPARAM(0), LPARAM(0)).0;
        if count < MIN_ITEMS_TO_LOG {
            return;
        }

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
            if copied < 0 {
                continue;
            }

            let bytes = &raw[..copied as usize];
            let (decoded, _, had_errors) = SHIFT_JIS.decode(bytes);
            if had_errors {
                continue;
            }

            eprintln!(
                "[combo-probe] hwnd=0x{:X} index={} text={}",
                hwnd.0 as usize, index, decoded
            );
        }
    }

    aviutl2::register_script_module!(ComboProbeModule);
}

#[cfg(not(windows))]
mod plugin {}
