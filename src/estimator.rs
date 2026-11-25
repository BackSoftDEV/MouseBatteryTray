//! Logic uoc tinh thuan (khong phu thuoc Windows).
//!
//! Y tuong: dem so giay chuot "hoat dong" ke tu lan sac cuoi, roi so voi
//! nguong do chinh nguoi dung hieu chinh (bam "Pin yeu" khi den bao nhay,
//! sau do bam "Vua sac xong"). Chua co nguong thi chi hien so gio da dung.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Khoang nghi giua 2 lan di/bam chuot duoc tinh la "van dang dung" toi da bao lau.
pub const DEFAULT_GAP_CAP_SECS: u64 = 300;
/// Chu ky ngan hon muc nay bi coi la nhap nham, khong dua vao hieu chinh.
pub const MIN_VALID_CYCLE_SECS: u64 = 3600;
/// So chu ky gan nhat dung de lay trung vi.
pub const CALIBRATION_WINDOW: usize = 5;
/// So chu ky toi da luu lai.
pub const MAX_CYCLES: usize = 10;
/// Tu muc nay cua nguong tro len thi coi la "sap het pin".
pub const WARN_PERCENT: u64 = 80;
/// Khoang cach toi thieu giua 2 lan ghi file khi dang dung chuot.
pub const SAVE_INTERVAL_SECS: u64 = 60;
/// Nguong mac dinh theo thong so hang Inphic F1S (500 mAh / 10 mA = 50 gio).
pub const DEFAULT_THRESHOLD_SECS: u64 = 50 * 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub version: u32,
    /// Thoi diem bam "Vua sac xong" (unix giay).
    pub charged_at: Option<u64>,
    /// So giay hoat dong tich luy ke tu lan sac cuoi.
    pub active_secs: u64,
    /// Lan tuong tac chuot gan nhat (unix giay).
    pub last_input: Option<u64>,
    /// So giay hoat dong luc nguoi dung bam "Pin yeu" trong chu ky hien tai.
    pub low_marked_at_secs: Option<u64>,
    /// Cac chu ky da hoan tat (co danh dau "Pin yeu" roi sac lai), cu -> moi.
    pub cycles: Vec<u64>,
    pub gap_cap_secs: u64,
    /// Nguong dat tay, chi dung khi chua co chu ky nao hoan tat.
    pub manual_threshold_secs: Option<u64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            charged_at: None,
            active_secs: 0,
            last_input: None,
            low_marked_at_secs: None,
            cycles: Vec::new(),
            gap_cap_secs: DEFAULT_GAP_CAP_SECS,
            manual_threshold_secs: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Chua co nguong: chi biet so gio da dung.
    Uncalibrated { used_secs: u64 },
    Healthy { used_secs: u64, remaining_secs: u64 },
    Warn { used_secs: u64, remaining_secs: u64 },
    Overdue { used_secs: u64, over_secs: u64 },
}

impl State {
    /// Ghi nhan 1 su kien chuot (chi can thoi diem).
    pub fn on_input(&mut self, now: u64) {
        if let Some(last) = self.last_input {
            if now < last {
                // Dong ho bi chinh lui: bo qua, chi cap nhat moc.
                self.last_input = Some(now);
                return;
            }
            self.active_secs += (now - last).min(self.gap_cap_secs);
        }
        self.last_input = Some(now);
    }

    /// Nguoi dung bam "Vua sac xong".
    pub fn mark_charged(&mut self, now: u64) {
        if let Some(low) = self.low_marked_at_secs.take() {
            self.cycles.push(low);
            if self.cycles.len() > MAX_CYCLES {
                let excess = self.cycles.len() - MAX_CYCLES;
                self.cycles.drain(..excess);
            }
        }
        self.active_secs = 0;
        self.last_input = None;
        self.charged_at = Some(now);
    }

    /// Nguoi dung bam "Pin yeu" (den bao nhay). Tra ve false neu bi bo qua.
    pub fn mark_low(&mut self) -> bool {
        if self.active_secs < MIN_VALID_CYCLE_SECS {
            return false;
        }
        self.low_marked_at_secs = Some(self.active_secs);
        true
    }

    /// Nguong (giay hoat dong) de coi la "pin yeu": trung vi cac chu ky gan nhat,
    /// hoac nguong dat tay neu chua co chu ky nao.
    pub fn threshold_secs(&self) -> Option<u64> {
        let mut window: Vec<u64> = self
            .cycles
            .iter()
            .rev()
            .take(CALIBRATION_WINDOW)
            .copied()
            .collect();
        if window.is_empty() {
            return self
                .manual_threshold_secs
                .filter(|&t| t >= MIN_VALID_CYCLE_SECS);
        }
        window.sort_unstable();
        let n = window.len();
        Some(if n % 2 == 1 {
            window[n / 2]
        } else {
            (window[n / 2 - 1] + window[n / 2]) / 2
        })
    }

    pub fn status(&self) -> Status {
        let used = self.active_secs;
        match self.threshold_secs() {
            None => Status::Uncalibrated { used_secs: used },
            Some(t) if used >= t => Status::Overdue {
                used_secs: used,
                over_secs: used - t,
            },
            Some(t) => {
                let remaining_secs = t - used;
                if used.saturating_mul(100) >= t.saturating_mul(WARN_PERCENT) {
                    Status::Warn {
                        used_secs: used,
                        remaining_secs,
                    }
                } else {
                    Status::Healthy {
                        used_secs: used,
                        remaining_secs,
                    }
                }
            }
        }
    }

    /// Da hoan tat it nhat 1 chu ky hieu chinh thuc te hay chua.
    pub fn is_calibrated(&self) -> bool {
        !self.cycles.is_empty()
    }

    /// % pin uoc tinh toi moc Pin yeu (None neu chua co nguong).
    pub fn estimated_percent(&self) -> Option<u8> {
        let t = self.threshold_secs()?;
        if self.active_secs >= t {
            return Some(0);
        }
        let rem = t - self.active_secs;
        let pct = (rem.saturating_mul(100) + t / 2) / t;
        Some(pct.clamp(1, 100) as u8)
    }

    /// Chuoi hien thi (tooltip NOTIFYICONDATAW gioi han 127 ky tu UTF-16).
    pub fn summary(&self, now: u64) -> String {
        let calibrated = self.is_calibrated();
        let base = match self.status() {
            Status::Uncalibrated { used_secs } => format!(
                "Chuột 2.4G: đã dùng {} giờ (chưa hiệu chỉnh)",
                fmt_hours(used_secs)
            ),
            Status::Healthy {
                used_secs,
                remaining_secs,
            } => {
                let pct = self.estimated_percent().unwrap_or(0);
                if calibrated {
                    format!(
                        "Chuột 2.4G: ~{}% tới mốc pin yếu · còn ~{} giờ (đã dùng {} giờ)",
                        pct,
                        fmt_hours(remaining_secs),
                        fmt_hours(used_secs)
                    )
                } else {
                    format!(
                        "Chuột 2.4G: ~{}% tới mốc pin yếu (mặc định) · còn ~{} giờ",
                        pct,
                        fmt_hours(remaining_secs)
                    )
                }
            }
            Status::Warn { remaining_secs, .. } => {
                let pct = self.estimated_percent().unwrap_or(0);
                if calibrated {
                    format!(
                        "Chuột 2.4G: ~{}% tới mốc pin yếu · sắp tới mốc sạc, còn ~{} giờ",
                        pct,
                        fmt_hours(remaining_secs)
                    )
                } else {
                    format!(
                        "Chuột 2.4G: ~{}% tới mốc pin yếu (mặc định) · còn ~{} giờ",
                        pct,
                        fmt_hours(remaining_secs)
                    )
                }
            }
            Status::Overdue {
                used_secs,
                over_secs,
            } => {
                if calibrated {
                    if over_secs == 0 {
                        "Chuột 2.4G: đã tới mốc pin yếu, nên sạc".to_string()
                    } else {
                        format!(
                            "Chuột 2.4G: quá mốc pin yếu {} giờ, nên sạc",
                            fmt_hours(over_secs)
                        )
                    }
                } else {
                    let threshold = self.threshold_secs().unwrap_or(DEFAULT_THRESHOLD_SECS);
                    format!(
                        "Chuột 2.4G: đã dùng {} giờ (vượt mặc định {} giờ, chưa hiệu chỉnh)",
                        fmt_hours(used_secs),
                        fmt_hours(threshold)
                    )
                }
            }
        };
        match self.charged_at {
            Some(c) if now >= c => {
                format!("{base} · sạc {} ngày trước", (now - c) / 86_400)
            }
            _ => base,
        }
    }

    /// Dong thong tin phu trong menu chuot phai.
    pub fn calibration_info(&self) -> String {
        match (self.cycles.len(), self.threshold_secs()) {
            (n, Some(t)) if n > 0 => format!(
                "Hiệu chỉnh: {n} chu kỳ · mốc pin yếu ~{} giờ{}",
                fmt_hours(t),
                if self.low_marked_at_secs.is_some() {
                    " · đã đánh dấu Pin yếu"
                } else {
                    ""
                }
            ),
            (_, Some(t)) => {
                if self.low_marked_at_secs.is_some() {
                    format!(
                        "Mặc định ~{} giờ (chưa hiệu chỉnh) · đã đánh dấu Pin yếu",
                        fmt_hours(t)
                    )
                } else {
                    format!(
                        "Mặc định ~{} giờ (500mAh, chưa hiệu chỉnh) · bấm Pin yếu khi đèn báo",
                        fmt_hours(t)
                    )
                }
            }
            _ => {
                if self.low_marked_at_secs.is_some() {
                    "Đã đánh dấu Pin yếu · bấm Vừa sạc xong khi sạc đầy".to_string()
                } else {
                    "Chưa hiệu chỉnh · bấm Pin yếu khi đèn chuột báo".to_string()
                }
            }
        }
    }

    fn sanitized(mut self) -> Self {
        if self.gap_cap_secs == 0 {
            self.gap_cap_secs = DEFAULT_GAP_CAP_SECS;
        }
        self.cycles.retain(|&c| c >= MIN_VALID_CYCLE_SECS);
        if self.cycles.len() > MAX_CYCLES {
            let excess = self.cycles.len() - MAX_CYCLES;
            self.cycles.drain(..excess);
        }
        self
    }
}

/// Thoi gian hien tai theo unix giay (0 neu dong ho he thong truoc nam 1970).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn fmt_hours(secs: u64) -> String {
    format!("{:.1}", secs as f64 / 3600.0).replace('.', ",")
}

/// Doc state tu file. Tra ve `(state, persist_blocked)`:
/// - File thieu (`NotFound`) -> mac dinh, cho phep ghi.
/// - File hong JSON -> doi ten thanh `.corrupt` roi dung mac dinh, cho phep ghi.
/// - Loi I/O khac (vd. file dang bi khoa) -> mac dinh, KHONG ghi de du lieu cu.
fn load_state_checked(path: &Path) -> (State, bool) {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<State>(&text) {
            Ok(state) => (state.sanitized(), false),
            Err(_) => {
                let _ = fs::rename(path, path.with_extension("corrupt"));
                (State::default(), false)
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => (State::default(), false),
        Err(_) => (State::default(), true),
    }
}

/// Doc state tu file.
pub fn load_state(path: &Path) -> State {
    load_state_checked(path).0
}

/// Ghi qua file tam roi doi ten de tranh hong file khi mat dien giua chung.
pub fn save_state(path: &Path, state: &State) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

/// Gan state voi file luu tru, ghi toi da 1 lan/phut khi dang dung chuot.
pub struct Tracker {
    pub state: State,
    path: PathBuf,
    last_save: Option<u64>,
    persist_blocked: bool,
}

impl Tracker {
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (state, persist_blocked) = load_state_checked(&path);
        Self {
            state,
            path,
            last_save: None,
            persist_blocked,
        }
    }

    pub fn is_persist_blocked(&self) -> bool {
        self.persist_blocked
    }

    /// Goi moi khi co su kien tu dung chuot. Loi ghi file chi tra ve de
    /// bo qua/ghi log, khong lam mat so lieu trong bo nho.
    pub fn on_input(&mut self, now: u64) -> io::Result<()> {
        self.state.on_input(now);
        let due = self
            .last_save
            .is_none_or(|t| now.saturating_sub(t) >= SAVE_INTERVAL_SECS);
        if due {
            self.flush(now)?;
        }
        Ok(())
    }

    pub fn mark_charged(&mut self, now: u64) -> io::Result<()> {
        self.state.mark_charged(now);
        self.flush(now)
    }

    /// Tra ve Ok(false) neu bi bo qua do chu ky qua ngan.
    pub fn mark_low(&mut self, now: u64) -> io::Result<bool> {
        if !self.state.mark_low() {
            return Ok(false);
        }
        self.flush(now)?;
        Ok(true)
    }

    pub fn flush(&mut self, now: u64) -> io::Result<()> {
        if self.persist_blocked {
            return Ok(());
        }
        save_state(&self.path, &self.state)?;
        self.last_save = Some(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("mbe-test-{}", std::process::id()))
            .join(name)
    }

    #[test]
    fn counts_gaps_up_to_cap() {
        let mut s = State::default();
        s.on_input(1000);
        s.on_input(1010);
        s.on_input(1100);
        assert_eq!(s.active_secs, 100);
    }

    #[test]
    fn long_gap_is_capped() {
        let mut s = State::default();
        s.on_input(0);
        s.on_input(10 * H);
        assert_eq!(s.active_secs, DEFAULT_GAP_CAP_SECS);
    }

    #[test]
    fn clock_going_backwards_is_ignored() {
        let mut s = State::default();
        s.on_input(1000);
        s.on_input(1060);
        s.on_input(500);
        assert_eq!(s.active_secs, 60);
        s.on_input(530);
        assert_eq!(s.active_secs, 90);
    }

    #[test]
    fn charge_resets_counters_but_keeps_history() {
        let mut s = State::default();
        s.active_secs = 50 * H;
        s.last_input = Some(5);
        assert!(s.mark_low());
        s.mark_charged(777);
        assert_eq!(s.active_secs, 0);
        assert_eq!(s.last_input, None);
        assert_eq!(s.charged_at, Some(777));
        assert_eq!(s.cycles, vec![50 * H]);
        assert_eq!(s.low_marked_at_secs, None);
    }

    #[test]
    fn charge_without_low_mark_records_no_cycle() {
        let mut s = State::default();
        s.active_secs = 30 * H;
        s.mark_charged(1);
        assert!(s.cycles.is_empty());
    }

    #[test]
    fn low_mark_too_early_is_rejected() {
        let mut s = State::default();
        s.active_secs = H - 1;
        assert!(!s.mark_low());
        assert_eq!(s.low_marked_at_secs, None);
    }

    #[test]
    fn pressing_low_twice_keeps_latest() {
        let mut s = State::default();
        s.active_secs = 40 * H;
        s.mark_low();
        s.active_secs = 45 * H;
        s.mark_low();
        s.mark_charged(1);
        assert_eq!(s.cycles, vec![45 * H]);
    }

    #[test]
    fn threshold_is_median_of_last_five() {
        let mut s = State::default();
        s.cycles = vec![1000 * H, 10 * H, 20 * H, 30 * H, 40 * H, 50 * H];
        assert_eq!(s.threshold_secs(), Some(30 * H));
        s.cycles = vec![20 * H, 40 * H];
        assert_eq!(s.threshold_secs(), Some(30 * H));
    }

    #[test]
    fn manual_threshold_used_only_before_first_cycle() {
        let mut s = State::default();
        s.manual_threshold_secs = Some(100 * H);
        assert_eq!(s.threshold_secs(), Some(100 * H));
        s.cycles = vec![60 * H];
        assert_eq!(s.threshold_secs(), Some(60 * H));
        s.cycles.clear();
        s.manual_threshold_secs = Some(5);
        assert_eq!(s.threshold_secs(), None);
    }

    #[test]
    fn status_and_estimated_percent_transitions() {
        let mut s = State::default();
        s.active_secs = 10 * H;
        assert_eq!(s.status(), Status::Uncalibrated { used_secs: 10 * H });
        assert_eq!(s.estimated_percent(), None);

        s.cycles = vec![100 * H];
        s.active_secs = 0;
        assert_eq!(s.estimated_percent(), Some(100));

        s.active_secs = 10 * H;
        assert_eq!(
            s.status(),
            Status::Healthy {
                used_secs: 10 * H,
                remaining_secs: 90 * H
            }
        );
        assert_eq!(s.estimated_percent(), Some(90));

        s.active_secs = 80 * H;
        assert_eq!(
            s.status(),
            Status::Warn {
                used_secs: 80 * H,
                remaining_secs: 20 * H
            }
        );
        assert_eq!(s.estimated_percent(), Some(20));

        s.active_secs = 79 * H;
        assert!(matches!(s.status(), Status::Healthy { .. }));

        s.active_secs = 100 * H;
        assert_eq!(
            s.status(),
            Status::Overdue {
                used_secs: 100 * H,
                over_secs: 0
            }
        );
        assert_eq!(s.estimated_percent(), Some(0));

        s.active_secs = 105 * H;
        assert_eq!(
            s.status(),
            Status::Overdue {
                used_secs: 105 * H,
                over_secs: 5 * H
            }
        );
        assert_eq!(s.estimated_percent(), Some(0));
    }

    #[test]
    fn summary_text_is_clear_and_fits_tooltip() {
        let mut s = State::default();
        s.active_secs = 42 * H + 1800;
        s.charged_at = Some(0);
        let text = s.summary(3 * 86_400 + 5);
        assert!(text.contains("Chuột 2.4G"), "{text}");
        assert!(text.contains("42,5 giờ"), "{text}");
        assert!(text.contains("chưa hiệu chỉnh"), "{text}");
        assert!(text.contains("sạc 3 ngày trước"), "{text}");
        assert!(text.encode_utf16().count() <= 127, "{text}");

        s.manual_threshold_secs = Some(50 * H);
        s.active_secs = 14 * H;
        let text_default = s.summary(3 * 86_400);
        assert!(!s.is_calibrated());
        assert!(text_default.contains("~72% tới mốc pin yếu (mặc định)"), "{text_default}");
        assert!(text_default.encode_utf16().count() <= 127, "{text_default}");

        s.active_secs = 52 * H;
        let text_default_over = s.summary(3 * 86_400);
        assert!(text_default_over.contains("vượt mặc định"), "{text_default_over}");
        assert!(text_default_over.encode_utf16().count() <= 127, "{text_default_over}");

        s.cycles = vec![100 * H];
        s.active_secs = 82 * H;
        let text = s.summary(3 * 86_400);
        assert!(s.is_calibrated());
        assert!(text.contains("~18% tới mốc pin yếu"), "{text}");
        assert!(text.contains("sắp tới mốc sạc"), "{text}");
        assert!(text.encode_utf16().count() <= 127, "{text}");

        s.active_secs = 102 * H;
        let text_over = s.summary(3 * 86_400);
        assert!(text_over.contains("quá mốc pin yếu"), "{text_over}");
        assert!(!text_over.contains("~0%"), "{text_over}");
        assert!(text_over.encode_utf16().count() <= 127, "{text_over}");
    }

    #[test]
    fn save_then_load_roundtrip() {
        let path = temp_path("roundtrip.json");
        let mut s = State::default();
        s.cycles = vec![55 * H, 60 * H];
        s.active_secs = 12 * H;
        s.charged_at = Some(123);
        save_state(&path, &s).unwrap();
        assert_eq!(load_state(&path), s);
    }

    #[test]
    fn corrupt_file_is_backed_up_not_overwritten() {
        let path = temp_path("corrupt.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corrupt = path.with_extension("corrupt");
        let _ = fs::remove_file(&corrupt);
        fs::write(&path, "{ day khong phai json").unwrap();
        let s = load_state(&path);
        assert_eq!(s, State::default());
        assert!(corrupt.exists());
        assert!(!path.exists());
    }

    #[test]
    fn unreadable_directory_path_blocks_persist() {
        let dir = temp_path("blocked_dir");
        fs::create_dir_all(&dir).unwrap();
        let t = Tracker::load(&dir);
        assert!(t.is_persist_blocked());
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let path = temp_path("partial.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"active_secs": 7200, "cycles": [10, 90000]}"#).unwrap();
        let s = load_state(&path);
        assert_eq!(s.active_secs, 7200);
        assert_eq!(s.gap_cap_secs, DEFAULT_GAP_CAP_SECS);
        assert_eq!(s.cycles, vec![90000]);
    }

    #[test]
    fn tracker_throttles_disk_writes() {
        let path = temp_path("tracker.json");
        let _ = fs::remove_file(&path);
        let mut t = Tracker::load(&path);
        t.on_input(1000).unwrap();
        assert!(path.exists());
        fs::remove_file(&path).unwrap();
        t.on_input(1030).unwrap();
        assert!(!path.exists());
        t.on_input(1061).unwrap();
        assert!(path.exists());
        assert_eq!(load_state(&path).active_secs, 61);
    }

    #[test]
    fn tracker_full_cycle_calibrates() {
        let path = temp_path("cycle.json");
        let _ = fs::remove_file(&path);
        let mut t = Tracker::load(&path);
        t.state.active_secs = 70 * H;
        assert!(t.mark_low(1).unwrap());
        t.mark_charged(2).unwrap();
        let reloaded = Tracker::load(&path);
        assert_eq!(reloaded.state.threshold_secs(), Some(70 * H));
        assert_eq!(reloaded.state.active_secs, 0);
    }
}
