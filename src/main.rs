#![windows_subsystem = "windows"]

use std::ffi::c_void;
use std::mem::size_of;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use kbd_battery_estimate::estimator::{
    unix_now, State, Status, Tracker, DEFAULT_THRESHOLD_SECS, MIN_VALID_CYCLE_SECS,
};
use kbd_battery_estimate::hid_probe::{log_hid_event, read_raw_b5_01, HidSample};
use kbd_battery_estimate::rawinput::{MouseFilter, MOUSE_DONGLE_ID};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT,
    POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetWindowLongPtrW, KillTimer, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, TrackPopupMenu,
    TranslateMessage, GWLP_USERDATA, HICON, ICONINFO, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, MF_UNCHECKED, MSG, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_ENDSESSION,
    WM_INPUT, WM_INPUT_DEVICE_CHANGE, WM_LBUTTONUP, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSW,
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_HID_RESULT: u32 = WM_APP + 2;
const TRAY_UID: u32 = 1;
const TIMER_REFRESH_ID: usize = 1;
const TIMER_REFRESH_MS: u32 = 60_000;
const HID_POLL_INTERVAL_SECS: u64 = 600; // 10 phut
const HID_STALL_HINT_SECS: u64 = 5;

const CMD_MARK_LOW: usize = 1001;
const CMD_MARK_CHARGED: usize = 1002;
const CMD_TOGGLE_STARTUP: usize = 1003;
const CMD_EXIT: usize = 1004;
const CMD_UNDO: usize = 1005;

const RUN_REG_SUBKEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_REG_VALUE: PCWSTR = w!("MouseBatteryTray");
const MUTEX_NAME: PCWSTR = w!("Local\\MouseBatteryTray_SingleInstance_Mutex");

struct HidProbeResult {
    sample: Option<HidSample>,
    event: &'static str,
    force_log: bool,
    finished_at: u64,
}

#[derive(Default)]
struct HidShared {
    in_flight: bool,
    started_at: u64,
    latest: Option<HidProbeResult>,
}

struct UndoSnapshot {
    label: &'static str,
    state: State,
}

struct AppState {
    hwnd: HWND,
    wm_taskbar_created: u32,
    tracker: Tracker,
    filter: MouseFilter,
    connected: bool,
    last_hid: Option<HidSample>,
    last_logged_hid: Option<HidSample>,
    last_hid_poll_sec: u64,
    hid_shared: Arc<Mutex<HidShared>>,
    probe_log_path: PathBuf,
    undo: Option<UndoSnapshot>,
    icon: HICON,
    last_input_sec: u64,
}

fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("MouseBatteryTray")
}

fn state_path() -> PathBuf {
    data_dir().join("mouse_estimate.json")
}

fn probe_log_path() -> PathBuf {
    data_dir().join("hid_probe.log")
}

fn load_mouse_tracker() -> Tracker {
    let mut tracker = Tracker::load(state_path());
    if tracker.state.cycles.is_empty() && tracker.state.manual_threshold_secs.is_none() {
        tracker.state.manual_threshold_secs = Some(DEFAULT_THRESHOLD_SECS);
        let _ = tracker.flush(unix_now());
    }
    tracker
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn run_once() {
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    let tracker = load_mouse_tracker();
    let filter = MouseFilter::new(MOUSE_DONGLE_ID);
    let connected = filter.is_connected();
    let now = unix_now();

    println!(
        "Dongle {} : {}",
        MOUSE_DONGLE_ID,
        if connected {
            "Đang kết nối"
        } else {
            "Chưa cắm"
        }
    );
    println!("{}", tracker.state.summary(now));
    println!("{}", tracker.state.calibration_info());
    match read_raw_b5_01() {
        Some(sample) => println!("{}", sample.menu_label()),
        None => println!("HID thô (B5 01): không đọc được / chưa cắm"),
    }
}

fn main() {
    if std::env::args().any(|a| a == "--once") {
        run_once();
        return;
    }

    // Single-instance guard.
    let mutex = match unsafe { CreateMutexW(None, false, MUTEX_NAME) } {
        Ok(h) => h,
        Err(_) => return,
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = unsafe { CloseHandle(mutex) };
        return;
    }

    run_tray_loop();
    let _ = unsafe { CloseHandle(mutex) };
}

fn run_tray_loop() {
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map(Into::into)
        .unwrap_or_default();
    let class_name = w!("MouseBatteryTrayHiddenWindow");

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: class_name,
        ..Default::default()
    };
    unsafe { RegisterClassW(&wc) };

    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("MouseBatteryTray"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(h) => h,
        Err(_) => return,
    };

    let wm_taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    let tracker = load_mouse_tracker();
    let filter = MouseFilter::new(MOUSE_DONGLE_ID);
    let connected = filter.is_connected();
    let icon = create_battery_icon(connected, &tracker);

    let mut app = Box::new(AppState {
        hwnd,
        wm_taskbar_created,
        tracker,
        filter,
        connected,
        last_hid: None,
        last_logged_hid: None,
        last_hid_poll_sec: 0,
        hid_shared: Arc::new(Mutex::new(HidShared::default())),
        probe_log_path: probe_log_path(),
        undo: None,
        icon,
        last_input_sec: 0,
    });

    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut *app as *mut AppState) as isize);
    }

    let _ = MouseFilter::register(hwnd);
    add_or_update_tray(&app, true);
    let _ = unsafe { SetTimer(Some(hwnd), TIMER_REFRESH_ID, TIMER_REFRESH_MS, None) };

    if app.connected {
        trigger_hid_probe(&mut app, "startup", false);
    }

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Giai phong sau khi vong lap ket thuc.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }
    if !app.icon.is_invalid() {
        let _ = unsafe { DestroyIcon(app.icon) };
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let app = unsafe { &mut *ptr };

    match msg {
        WM_INPUT => {
            if app.filter.is_target(lparam) {
                let now = unix_now();
                if now != app.last_input_sec {
                    app.last_input_sec = now;
                    let _ = app.tracker.on_input(now);
                }
            }
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        WM_INPUT_DEVICE_CHANGE => {
            app.filter.invalidate_cache();
            app.connected = app.filter.is_connected();
            if app.connected {
                trigger_hid_probe(app, "device_change", false);
            } else {
                app.last_hid = None;
            }
            update_tray_visual(app);
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == TIMER_REFRESH_ID => {
            app.connected = app.filter.is_connected();
            let now = unix_now();
            if app.connected {
                if now.saturating_sub(app.last_hid_poll_sec) >= HID_POLL_INTERVAL_SECS {
                    trigger_hid_probe(app, "poll", false);
                }
            } else {
                app.last_hid = None;
            }
            update_tray_visual(app);
            return LRESULT(0);
        }
        WM_HID_RESULT => {
            let result = app
                .hid_shared
                .lock()
                .ok()
                .and_then(|mut s| s.latest.take());
            if let Some(res) = result {
                app.last_hid = res.sample;
                app.last_hid_poll_sec = res.finished_at;
                log_hid_event(
                    &app.probe_log_path,
                    res.finished_at,
                    res.event,
                    res.sample,
                    res.force_log,
                    &mut app.last_logged_hid,
                );
            }
            return LRESULT(0);
        }
        WM_TRAY => {
            let ev = (lparam.0 & 0xFFFF) as u32;
            if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP {
                app.connected = app.filter.is_connected();
                if app.connected {
                    trigger_hid_probe(app, "tray_click", false);
                } else {
                    app.last_hid = None;
                }
                update_tray_visual(app);
                show_context_menu(app);
            }
            return LRESULT(0);
        }
        WM_COMMAND => {
            let cmd = wparam.0 & 0xFFFF;
            let now = unix_now();
            match cmd {
                CMD_MARK_LOW => {
                    let prev = app.tracker.state.clone();
                    if matches!(app.tracker.mark_low(now), Ok(true)) {
                        app.undo = Some(UndoSnapshot {
                            label: "Đánh dấu Pin yếu",
                            state: prev,
                        });
                        update_tray_visual(app);
                        trigger_hid_probe(app, "mark_low", true);
                    }
                }
                CMD_MARK_CHARGED => {
                    let prev = app.tracker.state.clone();
                    let _ = app.tracker.mark_charged(now);
                    app.undo = Some(UndoSnapshot {
                        label: "Vừa sạc xong",
                        state: prev,
                    });
                    update_tray_visual(app);
                    trigger_hid_probe(app, "mark_charged", true);
                }
                CMD_UNDO => {
                    if let Some(snap) = app.undo.take() {
                        app.tracker.state = snap.state;
                        let _ = app.tracker.flush(now);
                        update_tray_visual(app);
                        trigger_hid_probe(app, "undo", true);
                    }
                }
                CMD_TOGGLE_STARTUP => {
                    let enabled = is_startup_enabled();
                    set_startup_enabled(!enabled);
                }
                CMD_EXIT => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_QUERYENDSESSION => {
            let _ = app.tracker.flush(unix_now());
            return LRESULT(1);
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                let _ = app.tracker.flush(unix_now());
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            return LRESULT(0);
        }
        WM_DESTROY => {
            let _ = app.tracker.flush(unix_now());
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_REFRESH_ID) };
            remove_tray(hwnd);
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ if app.wm_taskbar_created != 0 && msg == app.wm_taskbar_created => {
            add_or_update_tray(app, true);
            return LRESULT(0);
        }
        _ => {}
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Ban 1 luong doc HID doc lap (toi da 1 lan do tai 1 thoi diem, khong block UI thread).
/// Neu dang co luong do khac chay hoac chua cam dongle ma `force_log == true`,
/// ghi ngay mau gan nhat vao log de khong mat dau su kien nguoi dung.
fn trigger_hid_probe(app: &mut AppState, event: &'static str, force_log: bool) {
    let now = unix_now();
    if !app.connected {
        if force_log {
            log_hid_event(
                &app.probe_log_path,
                now,
                event,
                None,
                true,
                &mut app.last_logged_hid,
            );
        }
        return;
    }

    let can_start = match app.hid_shared.lock() {
        Ok(mut guard) => {
            if guard.in_flight {
                false
            } else {
                guard.in_flight = true;
                guard.started_at = now;
                true
            }
        }
        Err(_) => false,
    };

    if !can_start {
        if force_log {
            log_hid_event(
                &app.probe_log_path,
                now,
                event,
                app.last_hid,
                true,
                &mut app.last_logged_hid,
            );
        }
        return;
    }

    let shared = Arc::clone(&app.hid_shared);
    let hwnd_raw = app.hwnd.0 as usize;

    std::thread::spawn(move || {
        let sample = read_raw_b5_01();
        let finished_at = unix_now();
        if let Ok(mut guard) = shared.lock() {
            guard.latest = Some(HidProbeResult {
                sample,
                event,
                force_log,
                finished_at,
            });
            guard.in_flight = false;
        }
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd_raw as *mut c_void)),
                WM_HID_RESULT,
                WPARAM(0),
                LPARAM(0),
            )
        };
    });
}

fn hid_is_stalled(app: &AppState, now: u64) -> bool {
    app.hid_shared
        .lock()
        .map(|g| g.in_flight && now.saturating_sub(g.started_at) >= HID_STALL_HINT_SECS)
        .unwrap_or(false)
}

fn update_tray_visual(app: &mut AppState) {
    let new_icon = create_battery_icon(app.connected, &app.tracker);
    if !new_icon.is_invalid() {
        let old = app.icon;
        app.icon = new_icon;
        add_or_update_tray(app, false);
        if !old.is_invalid() {
            let _ = unsafe { DestroyIcon(old) };
        }
    } else {
        add_or_update_tray(app, false);
    }
}

fn build_tooltip(app: &AppState) -> String {
    let summary = app.tracker.state.summary(unix_now());
    if app.connected {
        summary
    } else {
        format!("{summary} [chưa cắm dongle]")
    }
}

fn add_or_update_tray(app: &AppState, is_add: bool) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app.hwnd,
        uID: TRAY_UID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: app.icon,
        ..Default::default()
    };

    let tip = build_tooltip(app);
    let wide: Vec<u16> = tip.encode_utf16().take(nid.szTip.len() - 1).collect();
    nid.szTip[..wide.len()].copy_from_slice(&wide);
    nid.szTip[wide.len()] = 0;

    let msg = if is_add { NIM_ADD } else { NIM_MODIFY };
    unsafe {
        let _ = Shell_NotifyIconW(msg, &nid);
    }
}

fn remove_tray(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn show_context_menu(app: &AppState) {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };

    let now = unix_now();
    let status_line = if app.connected {
        app.tracker.state.summary(now)
    } else {
        format!("{} (chưa cắm dongle)", app.tracker.state.summary(now))
    };
    let calib_line = app.tracker.state.calibration_info();
    let stalled = hid_is_stalled(app, now);
    let hid_line = match app.last_hid {
        Some(sample) if stalled => format!("{} [đang dò...]", sample.menu_label()),
        Some(sample) => sample.menu_label(),
        None if stalled => "HID thô (B5 01): đang dò / thiết bị phản hồi chậm".to_string(),
        None if app.connected => "HID thô (B5 01): đang chờ đọc...".to_string(),
        None => "HID thô (B5 01): chưa cắm dongle 1EA7:0064".to_string(),
    };

    let can_mark_low = app.tracker.state.active_secs >= MIN_VALID_CYCLE_SECS;
    let mark_low_label = if can_mark_low {
        "Đánh dấu Pin yếu (khi đèn chuột báo)"
    } else {
        "Đánh dấu Pin yếu (cần dùng >= 1 giờ)"
    };
    let undo_label = app
        .undo
        .as_ref()
        .map(|snap| format!("Hoàn tác: {}", snap.label));
    let startup_checked = is_startup_enabled();

    unsafe {
        let w_status = to_wide(&status_line);
        let w_calib = to_wide(&calib_line);
        let w_hid = to_wide(&hid_line);
        let w_low = to_wide(mark_low_label);
        let w_charged = to_wide("Vừa sạc xong");
        let w_undo = undo_label.as_deref().map(to_wide);
        let w_startup = to_wide("Tự khởi động cùng Windows");
        let w_exit = to_wide("Thoát");

        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(w_status.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(w_calib.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(w_hid.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let low_flags = if can_mark_low {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        };
        let _ = AppendMenuW(menu, low_flags, CMD_MARK_LOW, PCWSTR(w_low.as_ptr()));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            CMD_MARK_CHARGED,
            PCWSTR(w_charged.as_ptr()),
        );
        if let Some(ref w_u) = w_undo {
            let _ = AppendMenuW(menu, MF_STRING, CMD_UNDO, PCWSTR(w_u.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let startup_flags = MF_STRING
            | if startup_checked {
                MF_CHECKED
            } else {
                MF_UNCHECKED
            };
        let _ = AppendMenuW(
            menu,
            startup_flags,
            CMD_TOGGLE_STARTUP,
            PCWSTR(w_startup.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, PCWSTR(w_exit.as_ptr()));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(app.hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            None,
            app.hwnd,
            None,
        );
        let _ = PostMessageW(Some(app.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

/// Ve icon vien pin 32x32 ARGB truc tiep tren DIB section (khong can file .ico).
fn create_battery_icon(connected: bool, tracker: &Tracker) -> HICON {
    const W: i32 = 32;
    const H: i32 = 32;

    // Mau BGRA (alpha premultiplied/straight cho tray icon 32-bit).
    let (fill_color, fill_ratio): ([u8; 4], f32) = if !connected {
        ([0x80, 0x80, 0x80, 0xFF], 0.0)
    } else if !tracker.state.is_calibrated() {
        // Chua hoan tat chu ky dau: giu mau trung tinh, khong bao cam/do.
        let ratio = tracker
            .state
            .estimated_percent()
            .map(|p| (p as f32 / 100.0).max(0.15))
            .unwrap_or(0.5);
        ([0xD4, 0x94, 0x52, 0xFF], ratio)
    } else {
        match tracker.state.status() {
            Status::Uncalibrated { .. } => ([0xD4, 0x94, 0x52, 0xFF], 0.5),
            Status::Healthy { .. } => {
                let pct = tracker.state.estimated_percent().unwrap_or(100) as f32 / 100.0;
                ([0x5E, 0xC5, 0x22, 0xFF], pct) // xanh la
            }
            Status::Warn { .. } => {
                let pct = tracker.state.estimated_percent().unwrap_or(20) as f32 / 100.0;
                ([0x0B, 0x9E, 0xF5, 0xFF], pct) // cam
            }
            Status::Overdue { .. } => ([0x44, 0x44, 0xEF, 0xFF], 0.15), // do
        }
    };

    let border_color: [u8; 4] = if connected {
        [0xF5, 0xF5, 0xF5, 0xFF]
    } else {
        [0x9A, 0x9A, 0x9A, 0xFF]
    };
    let bg_color: [u8; 4] = [0x24, 0x24, 0x24, 0xE6];

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: W,
            biHeight: -H, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits_ptr: *mut c_void = null_mut();
    let Ok(color_bmp) =
        (unsafe { CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) })
    else {
        return HICON::default();
    };
    if bits_ptr.is_null() {
        let _ = unsafe { DeleteObject(HGDIOBJ(color_bmp.0)) };
        return HICON::default();
    }

    let pixels = unsafe { std::slice::from_raw_parts_mut(bits_ptr as *mut u8, (W * H * 4) as usize) };
    pixels.fill(0);

    let set_px = |buf: &mut [u8], x: i32, y: i32, bgra: [u8; 4]| {
        if (0..W).contains(&x) && (0..H).contains(&y) {
            let idx = ((y * W + x) * 4) as usize;
            buf[idx..idx + 4].copy_from_slice(&bgra);
        }
    };

    // Dau cuc pin phia tren: x in 12..20, y in 2..5
    for y in 2..5 {
        for x in 12..20 {
            set_px(pixels, x, y, border_color);
        }
    }

    // Than pin: x in 7..25, y in 5..30 (vien day 2px)
    for y in 5..30 {
        for x in 7..25 {
            let is_border = x < 9 || x >= 23 || y < 7 || y >= 28;
            set_px(pixels, x, y, if is_border { border_color } else { bg_color });
        }
    }

    // Phan ruot mau ben trong: x in 10..22, y in 8..27 (chieu cao 19px)
    let inner_top = 8;
    let inner_bottom = 27;
    let inner_h = inner_bottom - inner_top;
    let fill_h = ((inner_h as f32) * fill_ratio.clamp(0.0, 1.0)).round() as i32;
    let fill_start = (inner_bottom - fill_h).max(inner_top);

    for y in fill_start..inner_bottom {
        for x in 10..22 {
            set_px(pixels, x, y, fill_color);
        }
    }

    let mask_bmp = unsafe { CreateBitmap(W, H, 1, 1, Some(null())) };
    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask_bmp,
        hbmColor: color_bmp,
    };
    let hicon = unsafe { CreateIconIndirect(&info) }.unwrap_or_default();

    let _ = unsafe { DeleteObject(HGDIOBJ(color_bmp.0)) };
    let _ = unsafe { DeleteObject(HGDIOBJ(mask_bmp.0)) };
    hicon
}

fn is_startup_enabled() -> bool {
    let mut hkey = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_REG_SUBKEY, None, KEY_READ, &mut hkey) }.0
        != 0
    {
        return false;
    }
    let mut size: u32 = 0;
    let ok = unsafe {
        RegQueryValueExW(hkey, RUN_REG_VALUE, None, None, None, Some(&mut size))
    }
    .0 == 0
        && size > 0;
    let _ = unsafe { RegCloseKey(hkey) };
    ok
}

fn set_startup_enabled(enable: bool) {
    let mut hkey = HKEY::default();
    if unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_REG_SUBKEY,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    }
    .0 != 0
    {
        return;
    }

    if enable {
        if let Ok(exe) = std::env::current_exe() {
            let quoted = format!("\"{}\"", exe.display());
            let wide = to_wide(&quoted);
            let bytes =
                unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
            let _ = unsafe { RegSetValueExW(hkey, RUN_REG_VALUE, None, REG_SZ, Some(bytes)) };
        }
    } else {
        let _ = unsafe { RegDeleteValueW(hkey, RUN_REG_VALUE) };
    }

    let _ = unsafe { RegCloseKey(hkey) };
}

#[allow(dead_code)]
fn _assert_handle_type(_: HANDLE) {}
