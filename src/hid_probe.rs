//! Doc thu dong 8 byte tho HID 0xB5 CMD 0x01 tu dongle chuot Inphic F1S
//! (VID_1EA7&PID_0064, UsagePage=0xFFB5, Usage=0x0001).
//!
//! Chi gui duy nhat lenh doc pin da biet (`B5 01 00...`), KHONG ghi cau hinh.
//! Dung de hien trong menu doi chieu va ghi log neu 8 byte co thay doi hoac khi bam nut.

use std::ffi::c_void;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::mem::size_of;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FlushQueue, HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid,
    HidD_GetPreparsedData, HidD_SetOutputReport, HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS,
    HIDP_STATUS_SUCCESS, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE,
    WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

const TARGET_VID: u16 = 0x1EA7;
const TARGET_PID: u16 = 0x0064;
const TARGET_USAGE_PAGE: u16 = 0xFFB5;
const TARGET_USAGE: u16 = 0x0001;
const READ_TIMEOUT_MS: u32 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidSample {
    pub bytes: [u8; 8],
}

impl HidSample {
    pub fn hex_string(&self) -> String {
        self.bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn menu_label(&self) -> String {
        let hex = self.hex_string();
        if self.bytes[0] == 0xB5 && self.bytes[1] == 0x01 && self.bytes[2] == 0x01 {
            format!("HID thô (B5 01): {hex} (byte[3]={}%)", self.bytes[3])
        } else {
            format!("HID thô (B5 01): {hex}")
        }
    }
}

/// Doc 1 mau 8 byte tu lenh `B5 01`. Tra ve None neu chua cam dongle hoac khong phan hoi.
pub fn read_raw_b5_01() -> Option<HidSample> {
    let guid = unsafe { HidD_GetHidGuid() };
    let dev_info = unsafe {
        SetupDiGetClassDevsW(
            Some(&guid),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    }
    .ok()?;

    let mut result = None;
    let mut index: u32 = 0;

    loop {
        let mut iface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInterfaces(dev_info, None, &guid, index, &mut iface) }.is_err()
        {
            break;
        }
        index += 1;

        let mut required: u32 = 0;
        let _ = unsafe {
            SetupDiGetDeviceInterfaceDetailW(dev_info, &iface, None, 0, Some(&mut required), None)
        };
        if required == 0 {
            continue;
        }

        let mut buf = vec![0u8; required as usize];
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                dev_info,
                &iface,
                Some(detail),
                required,
                None,
                None,
            )
        }
        .is_err()
        {
            continue;
        }

        let path_ptr = unsafe { (*detail).DevicePath.as_ptr() };
        if let Some(sample) = probe_device_path(PCWSTR(path_ptr)) {
            result = Some(sample);
            break;
        }
    }

    let _ = unsafe { SetupDiDestroyDeviceInfoList(dev_info) };
    result
}

fn probe_device_path(path: PCWSTR) -> Option<HidSample> {
    let handle = unsafe {
        CreateFileW(
            path,
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .ok()?;

    let sample = probe_open_handle(handle);
    let _ = unsafe { CloseHandle(handle) };
    sample
}

fn probe_open_handle(handle: HANDLE) -> Option<HidSample> {
    let mut attrs = HIDD_ATTRIBUTES {
        Size: size_of::<HIDD_ATTRIBUTES>() as u32,
        ..Default::default()
    };
    if !unsafe { HidD_GetAttributes(handle, &mut attrs) } {
        return None;
    }
    if attrs.VendorID != TARGET_VID || attrs.ProductID != TARGET_PID {
        return None;
    }

    let mut preparsed = PHIDP_PREPARSED_DATA::default();
    if !unsafe { HidD_GetPreparsedData(handle, &mut preparsed) } {
        return None;
    }
    let mut caps = HIDP_CAPS::default();
    let caps_ok = unsafe { HidP_GetCaps(preparsed, &mut caps) } == HIDP_STATUS_SUCCESS;
    let _ = unsafe { HidD_FreePreparsedData(preparsed) };
    if !caps_ok || caps.UsagePage != TARGET_USAGE_PAGE || caps.Usage != TARGET_USAGE {
        return None;
    }

    // Xa hang doi input cu truoc khi gui lenh doc de tranh doc nham goi cu.
    let _ = unsafe { HidD_FlushQueue(handle) };

    let out_len = (caps.OutputReportByteLength as usize).max(8);
    let in_len = (caps.InputReportByteLength as usize).max(8);
    let mut out_buf = vec![0u8; out_len];
    out_buf[0] = 0xB5;
    out_buf[1] = 0x01;

    if !unsafe { HidD_SetOutputReport(handle, out_buf.as_ptr() as *const c_void, out_len as u32) } {
        return None;
    }

    let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()?;
    let deadline = Instant::now() + Duration::from_millis(READ_TIMEOUT_MS as u64);
    let mut sample = None;

    loop {
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u32;
        if remaining_ms == 0 {
            break;
        }

        let _ = unsafe { ResetEvent(event) };
        let mut overlapped = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        let mut in_buf = vec![0u8; in_len];
        let mut read_bytes: u32 = 0;

        let read_res = unsafe {
            ReadFile(
                handle,
                Some(&mut in_buf),
                Some(&mut read_bytes),
                Some(&mut overlapped),
            )
        };

        let got_data = match read_res {
            Ok(()) => true,
            Err(_) => {
                if unsafe { GetLastError() } == ERROR_IO_PENDING {
                    if unsafe { WaitForSingleObject(event, remaining_ms) } == WAIT_OBJECT_0 {
                        unsafe { GetOverlappedResult(handle, &overlapped, &mut read_bytes, false) }
                            .is_ok()
                    } else {
                        let _ = unsafe { CancelIo(handle) };
                        let mut ignored: u32 = 0;
                        let _ =
                            unsafe { GetOverlappedResult(handle, &overlapped, &mut ignored, true) };
                        false
                    }
                } else {
                    false
                }
            }
        };

        if !got_data {
            break;
        }
        // Chi chap nhan goi phan hoi dung header B5 01.
        if read_bytes >= 8 && in_buf[0] == 0xB5 && in_buf[1] == 0x01 {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&in_buf[..8]);
            sample = Some(HidSample { bytes });
            break;
        }
    }

    let _ = unsafe { CloseHandle(event) };
    sample
}

fn local_time_string() -> String {
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// Ghi log HID kem moc thoi gian va ten su kien.
/// - `force == false` (poll/startup): chi ghi khi 8 byte thay doi so voi `last_logged`.
/// - `force == true` (mark_low/mark_charged/undo): luon ghi de doi chieu voi thao tac nguoi dung.
pub fn log_hid_event(
    path: &Path,
    now_unix: u64,
    event: &str,
    sample: Option<HidSample>,
    force: bool,
    last_logged: &mut Option<HidSample>,
) {
    if !force && (sample.is_none() || *last_logged == sample) {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let time_str = local_time_string();
        match sample {
            Some(s) => {
                let _ = writeln!(
                    file,
                    "time={} ts={} event={} raw={} byte3={}",
                    time_str,
                    now_unix,
                    event,
                    s.hex_string(),
                    s.bytes[3]
                );
                *last_logged = Some(s);
            }
            None => {
                let _ = writeln!(
                    file,
                    "time={} ts={} event={} raw=NONE",
                    time_str, now_unix, event
                );
            }
        }
    }
}
