//! Uoc tinh pin chuot 2.4G (Inphic F1S, VID_1EA7&PID_0064) theo thoi gian dung.
//!
//! - `estimator`: logic thuan (khong phu thuoc Windows), co unit test.
//! - `rawinput`: nhan biet di chuyen/click tu dung dongle chuot (chi Windows).
//! - `hid_probe`: doc thu dong 8 byte tho HID 0xB5 CMD 0x01 de doi chieu (chi Windows).

pub mod estimator;

#[cfg(windows)]
pub mod rawinput;

#[cfg(windows)]
pub mod hid_probe;

pub use estimator::{unix_now, State, Status, Tracker};
