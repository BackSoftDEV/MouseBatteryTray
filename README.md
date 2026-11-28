# MouseBatteryTray

Ứng dụng Windows system tray nhỏ gọn viết bằng Rust để ước lượng thời lượng pin còn lại cho chuột không dây 2.4G **Inphic F1S** (`VID_1EA7&PID_0064`) dựa trên thời gian sử dụng thực tế và chu kỳ hiệu chỉnh của chính người dùng.

---

## Vì sao dùng ước lượng thay vì đọc % pin trực tiếp?

Qua kiểm tra trực tiếp HID descriptor và đo thực tế trên dongle `VID_1EA7&PID_0064` (Inphic F1S):

- Dongle chỉ có 2 HID collection (`UsagePage=0xFFB5/0x0001` và chuột HID chuẩn `0x0001/0x0002`), không có HID Battery UsagePage (`0x0085` / `0x0006`) và không có Feature Report.
- Lệnh đọc vendor `ReportID 0xB5` (`B5 01`) luôn trả về `byte[3] = 0x38` (`56%`) cố định — kể cả khi rút/cắm lại dongle lúc chuột đang tắt hoàn toàn hay sau nhiều ngày sử dụng; 4 byte sau (`byte[4..7]`) là dữ liệu bộ đệm RAM chưa xóa của firmware.
- Cổng sạc USB-C trên chuột là loại chỉ truyền nguồn (power-only), sóng 2.4G không báo trạng thái đang sạc và dongle cũng không báo khi chuột gạt công tắc tắt.

Vì vậy, phương án đáng tin cậy nhất từ phía Windows là **đếm thời gian hoạt động thực tế của đúng dongle chuột** kết hợp **hiệu chỉnh theo mốc đèn báo pin yếu thực tế**.

---

## Tính năng chính

- **Đếm đúng thiết bị mục tiêu**: Dùng Windows Raw Input (`RIDEV_INPUTSINK | RIDEV_DEVNOTIFY`) lọc đúng `VID_1EA7&PID_0064`, chỉ đọc `RAWINPUTHEADER` (không đọc tọa độ, không đọc thao tác click/phím, bỏ qua sự kiện giả lập `hDevice == NULL`).
- **Tích lũy thời gian thông minh**: Chỉ cộng khoảng thời gian giữa 2 lần thao tác với mức trần mỗi khoảng nghỉ `300 giây` (5 phút), ghi file định kỳ mỗi `60 giây`.
- **Hiệu chỉnh thực tế & trung thực**:
  - Trước khi có chu kỳ đầu tiên: dùng mốc mặc định `50,0 giờ` (500 mAh / 10 mA) với **icon màu xanh-xám trung tính** và nhãn `(mặc định)` để tránh báo động giả.
  - Sau khi hoàn tất chu kỳ thực tế: tự động lấy **trung vị tối đa 5 chu kỳ gần nhất** (lưu tối đa 10 chu kỳ, bỏ qua chu kỳ ngắn hơn 1 giờ) và hiển thị icon theo màu trực quan (xanh lá → cam từ 80% mốc → đỏ khi vượt mốc).
- **Không làm phiền**: Không hiện popup/toast/balloon; kiểm tra pin trực tiếp qua tooltip và menu chuột phải trên khay hệ thống.
- **Hoàn tác 1 bước**: Cho phép hoàn tác ngay trong menu nếu lỡ bấm nhầm **Đánh dấu Pin yếu** hoặc **Vừa sạc xong**.
- **Bền bỉ khi chạy nền**: Single-instance mutex, tự khôi phục icon khi `explorer.exe` khởi động lại (`TaskbarCreated`), hỗ trợ bật/tắt **Tự khởi động cùng Windows** qua Registry (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run\MouseBatteryTray`).
- **Ghi log đối chiếu HID `B5 01` thụ động**: Chỉ gửi duy nhất lệnh đọc an toàn `B5 01` trên luồng nền (không chặn UI, mỗi 10 phút và khi có sự kiện mở menu/cắm rút/bấm nút), lưu log khi 8 byte thay đổi hoặc khi người dùng đánh dấu.
- **Chế độ kiểm tra nhanh qua dòng lệnh**: `MouseBatteryTray.exe --once`.

---

## Cách sử dụng

1. Chạy `MouseBatteryTray.exe` (icon viên pin sẽ xuất hiện trên khay hệ thống).
2. Di chuột vào icon để xem nhanh trạng thái pin; nhấp phải để mở menu.
3. Trong chu kỳ đầu tiên, app hiển thị mốc mặc định `~50,0 giờ`.
4. Khi đèn báo pin yếu trên chuột nháy: mở menu tray → bấm **Đánh dấu Pin yếu (khi đèn chuột báo)** *(chỉ khả dụng sau khi đã dùng ít nhất 1 giờ)*.
5. Sau khi sạc đầy chuột: mở menu tray → bấm **Vừa sạc xong** để chốt chu kỳ hiệu chỉnh và đặt lại bộ đếm về `0`.
6. Nếu lỡ bấm nhầm ở bước 4 hoặc 5: chọn **Hoàn tác** trong menu tray để khôi phục trạng thái ngay trước đó.
7. Tùy chọn tích **Tự khởi động cùng Windows** trong menu tray.

---

## Build & Kiểm thử

### Yêu cầu
- Windows 10 / 11
- [Rust toolchain](https://rustup.rs/) (`cargo`, `rustc`)

### Lệnh build và chạy test

```powershell
# Chạy toàn bộ unit test (logic ước lượng & hiệu chỉnh)
cargo test

# Build bản release
cargo build --release
```

File thực thi sau khi build nằm tại:

```text
target\release\MouseBatteryTray.exe
```

Kiểm tra nhanh trạng thái hiện tại trên terminal (PowerShell):

```powershell
.\target\release\MouseBatteryTray.exe --once | Out-String
```

### Tùy chỉnh cho chuột 2.4G khác

- Đổi hằng số `MOUSE_DONGLE_ID` trong `src/rawinput.rs` theo `VID_xxxx&PID_xxxx` của dongle chuột bạn đang dùng.
- Nếu muốn thay đổi đối tượng đọc HID `B5 01`, cập nhật `TARGET_VID` và `TARGET_PID` trong `src/hid_probe.rs`.

---

## Dữ liệu lưu trữ & Quyền riêng tư

Tất cả dữ liệu được lưu cục bộ trong `%LOCALAPPDATA%\MouseBatteryTray\`:

| File | Mục đích |
|---|---|
| `mouse_estimate.json` | Lưu thời gian sử dụng tích lũy, mốc sạc gần nhất và lịch sử các chu kỳ hiệu chỉnh |
| `hid_probe.log` | Ghi nhận 8 byte phản hồi thô từ lệnh đọc `B5 01` kèm thời gian và tên sự kiện |

Ứng dụng chạy hoàn toàn offline, không gửi dữ liệu ra ngoài và không đọc nội dung bàn phím hay vị trí con trỏ chuột.

---

## Cấu trúc mã nguồn

```text
src/
├── main.rs        # Cửa sổ ẩn Win32, tray icon 32x32 DIB, menu, worker HID nền, --once
├── lib.rs         # Khai báo module dùng chung cho binary và unit test
├── estimator.rs   # Logic đếm giờ, hiệu chỉnh trung vị, trạng thái/chuỗi hiển thị & unit test
├── rawinput.rs    # Lọc sự kiện chuột theo VID/PID qua Windows Raw Input
└── hid_probe.rs   # Đọc thụ động 8 byte HID B5 01 (overlapped I/O) và ghi hid_probe.log
```
