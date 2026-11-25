//! Nhan biet su kien chuot den tu dung dongle 2.4G bang Raw Input (chi Windows).
//!
//! Rieng tu: chi doc `RAWINPUTHEADER` (loai thiet bi + handle), KHONG doc toa do
//! hay noi dung. Bo qua su kien gia lap (`hDevice == NULL`).

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RegisterRawInputDevices,
    HRAWINPUT, RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RIDEV_DEVNOTIFY,
    RIDEV_INPUTSINK, RIDI_DEVICENAME, RID_HEADER, RIM_TYPEMOUSE,
};

/// VID/PID cua dongle chuot 2.4G Inphic F1S (khop khong phan biet hoa thuong).
pub const MOUSE_DONGLE_ID: &str = "VID_1EA7&PID_0064";

pub struct MouseFilter {
    needle: String,
    /// handle thiet bi -> co phai chuot can theo doi hay khong.
    cache: HashMap<isize, bool>,
}

impl MouseFilter {
    pub fn new(vid_pid: &str) -> Self {
        Self {
            needle: vid_pid.to_uppercase(),
            cache: HashMap::new(),
        }
    }

    /// Dang ky nhan WM_INPUT cua moi chuot ke ca khi cua so an (RIDEV_INPUTSINK)
    /// va nhan WM_INPUT_DEVICE_CHANGE khi cam/rut thiet bi.
    pub fn register(hwnd: HWND) -> windows::core::Result<()> {
        let devices = [RAWINPUTDEVICE {
            usUsagePage: 0x01, // Generic Desktop
            usUsage: 0x02,     // Mouse
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        }];
        unsafe { RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32) }
    }

    /// Xoa bo nho dem handle; goi khi nhan WM_INPUT_DEVICE_CHANGE.
    pub fn invalidate_cache(&mut self) {
        self.cache.clear();
    }

    /// `lparam` cua WM_INPUT. True neu su kien den tu dung chuot can theo doi.
    pub fn is_target(&mut self, lparam: LPARAM) -> bool {
        let Some(header) = read_header(lparam) else {
            return false;
        };
        if header.dwType != RIM_TYPEMOUSE.0 || header.hDevice.0.is_null() {
            return false;
        }
        let key = header.hDevice.0 as isize;
        if let Some(&hit) = self.cache.get(&key) {
            return hit;
        }
        let hit = device_name(header.hDevice)
            .map(|name| name.to_uppercase().contains(&self.needle))
            .unwrap_or(false);
        self.cache.insert(key, hit);
        hit
    }

    /// Kiem tra hien tai dongle chuot co dang cam tren he thong khong.
    pub fn is_connected(&self) -> bool {
        let mut count: u32 = 0;
        let elem_size = size_of::<RAWINPUTDEVICELIST>() as u32;
        let r = unsafe { GetRawInputDeviceList(None, &mut count, elem_size) };
        if r == u32::MAX || count == 0 {
            return false;
        }
        let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
        let got = unsafe { GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut count, elem_size) };
        if got == u32::MAX {
            return false;
        }
        list.into_iter().take(got as usize).any(|dev| {
            dev.dwType == RIM_TYPEMOUSE
                && !dev.hDevice.0.is_null()
                && device_name(dev.hDevice)
                    .map(|name| name.to_uppercase().contains(&self.needle))
                    .unwrap_or(false)
        })
    }
}

fn read_header(lparam: LPARAM) -> Option<RAWINPUTHEADER> {
    let mut header = RAWINPUTHEADER::default();
    let header_size = size_of::<RAWINPUTHEADER>() as u32;
    let mut size = header_size;
    let copied = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut c_void),
            RID_HEADER,
            Some(&mut header as *mut RAWINPUTHEADER as *mut c_void),
            &mut size,
            header_size,
        )
    };
    if copied == u32::MAX || copied < header_size {
        None
    } else {
        Some(header)
    }
}

fn device_name(device: HANDLE) -> Option<String> {
    let mut len: u32 = 0;
    let r = unsafe { GetRawInputDeviceInfoW(Some(device), RIDI_DEVICENAME, None, &mut len) };
    if r == u32::MAX || len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    let copied = unsafe {
        GetRawInputDeviceInfoW(
            Some(device),
            RIDI_DEVICENAME,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut len,
        )
    };
    if copied == u32::MAX || copied == 0 {
        return None;
    }
    let n = (copied as usize).min(buf.len());
    Some(
        String::from_utf16_lossy(&buf[..n])
            .trim_end_matches('\0')
            .to_string(),
    )
}
