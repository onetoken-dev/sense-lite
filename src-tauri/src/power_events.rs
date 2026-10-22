//! 监听 Windows 电源事件:显示器从息屏恢复点亮时触发回调。
//!
//! 用途:宏碁的蓝光过滤(gamma/色温变换)会被息屏时的显卡颜色管线重置冲掉,
//! 需要在亮屏后重新下发。通过 RegisterPowerSettingNotification 监听
//! GUID_MONITOR_POWERED_ON,在一个消息-only 窗口线程里接收 WM_POWERBROADCAST。

#[cfg(windows)]
pub mod monitor {
    #![allow(non_snake_case, non_camel_case_types, dead_code)]

    use std::ffi::c_void;
    use std::sync::OnceLock;
    use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

    const HWND_MESSAGE: isize = -3;
    const WM_POWERBROADCAST: u32 = 0x0218;
    const PBT_POWERSETTINGCHANGE: usize = 0x8013;
    const DEVICE_NOTIFY_WINDOW_HANDLE: u32 = 0;

    /// GUID_MONITOR_POWERED_ON {02731015-4510-4a26-8ab8-e274d07a7bf9}
    const GUID_MONITOR_POWERED_ON: GUID = GUID {
        Data1: 0x02731015,
        Data2: 0x4510,
        Data3: 0x4a26,
        Data4: [0x8a, 0xb8, 0xe2, 0x74, 0xd0, 0x7a, 0x7b, 0xf9],
    };

    #[repr(C)]
    #[derive(Clone, Copy, PartialEq)]
    struct GUID {
        Data1: u32,
        Data2: u16,
        Data3: u16,
        Data4: [u8; 8],
    }

    #[repr(C)]
    struct POWERBROADCAST_SETTING {
        Guid: GUID,
        /// 实为 DWORD 数组首元素;显示器状态 0=息屏 1=点亮
        Data: u32,
    }

    type LRESULT = isize;
    type HWND = isize;
    type WNDPROC = unsafe extern "system" fn(HWND, u32, usize, isize) -> LRESULT;

    #[repr(C)]
    struct WNDCLASSW {
        style: u32,
        lpfnWndProc: WNDPROC,
        cbClsExtra: i32,
        cbWndExtra: i32,
        hInstance: isize,
        hIcon: isize,
        hCursor: isize,
        hbrBackground: isize,
        lpszMenuName: *const u16,
        lpszClassName: *const u16,
    }

    #[repr(C)]
    struct MSG {
        hwnd: HWND,
        message: u32,
        wParam: usize,
        lParam: isize,
        time: u32,
        pt_x: i32,
        pt_y: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(lpWndClass: *const WNDCLASSW) -> u16;
        fn CreateWindowExW(
            dwExStyle: u32,
            lpClassName: *const u16,
            lpWindowName: *const u16,
            dwStyle: u32,
            x: i32,
            y: i32,
            nWidth: i32,
            nHeight: i32,
            hWndParent: HWND,
            hMenu: isize,
            hInstance: isize,
            lpParam: *mut c_void,
        ) -> HWND;
        fn GetMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: u32, wMsgFilterMax: u32) -> i32;
        fn TranslateMessage(lpMsg: *const MSG) -> i32;
        fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
        fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: usize, lParam: isize) -> LRESULT;
        fn RegisterPowerSettingNotification(
            hWnd: HWND,
            PowerSettingGuid: *const GUID,
            Flags: u32,
        ) -> isize;
        fn GetModuleHandleW(lpModuleName: *const u16) -> isize;
    }

    static SENDER: OnceLock<UnboundedSender<()>> = OnceLock::new();

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> LRESULT {
        unsafe {
            if msg == WM_POWERBROADCAST && wparam == PBT_POWERSETTINGCHANGE {
                let ps = lparam as *const POWERBROADCAST_SETTING;
                if !ps.is_null() && (*ps).Guid == GUID_MONITOR_POWERED_ON && (*ps).Data != 0 {
                    if let Some(tx) = SENDER.get() {
                        let _ = tx.send(());
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
    }

    use std::os::windows::ffi::OsStrExt;

    /// 启动监听线程。每次显示器点亮,返回的接收器收到一个 `()`。
    pub fn spawn_monitor_on_events() -> std::io::Result<UnboundedReceiver<()>> {
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        SENDER
            .set(tx)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::AlreadyExists, "already spawned"))?;

        std::thread::Builder::new()
            .name("power-events".into())
            .spawn(move || unsafe {
                let class_name = wide("sense_lite_powerwnd");
                let hinstance = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: wndproc,
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinstance,
                    hIcon: 0,
                    hCursor: 0,
                    hbrBackground: 0,
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class_name.as_ptr(),
                };
                if RegisterClassW(&wc) == 0 {
                    return;
                }
                let hwnd = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    class_name.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    0,
                    hinstance,
                    std::ptr::null_mut(),
                );
                if hwnd == 0 {
                    return;
                }
                if RegisterPowerSettingNotification(
                    hwnd,
                    &GUID_MONITOR_POWERED_ON,
                    DEVICE_NOTIFY_WINDOW_HANDLE,
                ) == 0
                {
                    return;
                }
                let mut msg = MSG {
                    hwnd: 0,
                    message: 0,
                    wParam: 0,
                    lParam: 0,
                    time: 0,
                    pt_x: 0,
                    pt_y: 0,
                };
                while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            })?;

        Ok(rx)
    }
}

#[cfg(windows)]
pub use monitor::spawn_monitor_on_events;

#[cfg(not(windows))]
pub fn spawn_monitor_on_events() -> std::io::Result<std::convert::Infallible> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "power events are Windows-only",
    ))
}
