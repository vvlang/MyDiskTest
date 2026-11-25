#[cfg(target_os = "windows")]
use crate::chip::wdb::{is_known_nand_vendor, nand_manufacturer_name};
use crate::chip::wdb::{ChipDatabase, ControllerMatch, FlashRecord};
use crate::disk::DiskInfo;

#[derive(Debug, Clone)]
pub struct ChipInspection {
    pub vid: Option<String>,
    pub pid: Option<String>,
    pub bcd_device: Option<String>,
    pub scsi_vendor: Option<String>,
    pub scsi_product: Option<String>,
    pub scsi_rev: Option<String>,
    pub controller_match: Option<ControllerMatch>,
    /// Exact controller string read directly from firmware via vendor SCSI command (Windows)
    pub firmware_controller_hint: Option<String>,
    /// Raw NAND Flash ID bytes in uppercase spaced hex (e.g. "98 3A 98 B3 76 72")
    pub flash_id_hex: Option<String>,
    /// Decoded NAND flash record matched from Chips.wdb
    pub flash_record: Option<FlashRecord>,
    /// Fallback NAND manufacturer if byte 0 is recognized but exact ID is newer than Chips.wdb
    pub flash_vendor_fallback: Option<String>,
    /// Explanation note (e.g. macOS kernel SCSI restriction or Windows admin prompt tip)
    pub probe_status: String,
}

impl ChipInspection {
    /// Generates a classic ChipGenius-style copyable Chinese hardware report.
    pub fn format_chipgenius_report(&self, disk: &DiskInfo) -> String {
        let mut s = String::with_capacity(512);
        s.push_str(&format!("设备描述: {}\n", disk.name));
        s.push_str(&format!("设备类型: {}\n", disk.kind.label_zh()));
        s.push_str(&format!("协议版本: {}\n", disk.protocol));
        s.push_str(&format!("挂载路径: {}\n", disk.mount_point.display()));
        if let (Some(ref vid), Some(ref pid)) = (&self.vid, &self.pid) {
            s.push_str(&format!("设备 ID: VID = {}  PID = {}\n", vid, pid));
        }
        if let Some(ref bcd) = self.bcd_device {
            s.push_str(&format!("设备版本: {}\n", bcd));
        }
        if !disk.vendor.is_empty() {
            s.push_str(&format!("设备厂商: {}\n", disk.vendor));
        }
        if !disk.model.is_empty() {
            s.push_str(&format!("产品型号: {}\n", disk.model));
        }
        if let Some(ref sn) = disk.serial {
            s.push_str(&format!("产品序列号: {}\n", sn));
        }
        if let (Some(ref sv), Some(ref sp)) = (&self.scsi_vendor, &self.scsi_product) {
            s.push_str(&format!(
                "SCSI 信息: {} / {} (Rev {})\n",
                sv,
                sp,
                self.scsi_rev.as_deref().unwrap_or("-")
            ));
        }

        if let Some(ref m) = self.controller_match {
            s.push_str(&format!(
                "芯片厂商: {}\n",
                if m.primary.chip_vendor.is_empty() {
                    "未知"
                } else {
                    &m.primary.chip_vendor
                }
            ));
            let ctrl_display = self
                .firmware_controller_hint
                .as_deref()
                .filter(|c| !c.is_empty())
                .unwrap_or(if m.primary.controller.is_empty() {
                    "未知（仅匹配厂商 VID）"
                } else {
                    &m.primary.controller
                });
            s.push_str(&format!("芯片型号: {}\n", ctrl_display));
            if !m.primary.note.is_empty() {
                s.push_str(&format!("数据库备注: {}\n", m.primary.note));
            }
            if !m.primary.url.is_empty() {
                s.push_str(&format!("参考资料: {}\n", m.primary.url));
            }
        } else if let Some(ref hint) = self.firmware_controller_hint {
            s.push_str(&format!("芯片型号: {}\n", hint));
        } else {
            s.push_str("芯片型号: 未在 Chips.wdb 中命中（可能是内置盘、苹果原厂盘或未收录的新款主控）\n");
        }

        if let Some(ref fid) = self.flash_id_hex {
            s.push_str(&format!("闪存识别码: {}\n", fid));
        }
        if let Some(ref fr) = self.flash_record {
            s.push_str(&format!("闪存厂商: {}\n", fr.manufacturer));
            s.push_str(&format!("颗粒类型: {}\n", fr.flash_type));
            if !fr.part_numbers.is_empty() {
                s.push_str(&format!("闪存型号: {}\n", fr.part_numbers.join(" / ")));
            }
            if fr.is_downgraded {
                s.push_str("⚠️ 警告: 检测到 SpecTek (大S) 降级/白片闪存颗粒！\n");
            }
        } else if let Some(ref v) = self.flash_vendor_fallback {
            s.push_str(&format!("闪存厂商: {}（Chips.wdb 暂未收录该精确 ID）\n", v));
        } else {
            s.push_str(&format!("闪存探测状态: {}\n", self.probe_status));
        }

        s
    }
}

/// Performs USB controller & NAND flash inspection for the selected drive using `Chips.wdb`
/// plus platform-native hardware probing (Windows SCSI Pass-Through / macOS USB descriptor matching).
pub fn probe_disk_chip(disk: &DiskInfo, db: &ChipDatabase) -> ChipInspection {
    let vid = disk.vid.clone();
    let pid = disk.pid.clone();
    let bcd = disk.bcd_device.clone();

    let controller_match = match (&vid, &pid) {
        (Some(v), Some(p)) => db.lookup_controller(v, p, bcd.as_deref()),
        _ => None,
    };

    let mut insp = ChipInspection {
        vid,
        pid,
        bcd_device: bcd,
        scsi_vendor: None,
        scsi_product: None,
        scsi_rev: None,
        controller_match,
        firmware_controller_hint: None,
        flash_id_hex: None,
        flash_record: None,
        flash_vendor_fallback: None,
        probe_status: String::new(),
    };

    #[cfg(target_os = "windows")]
    {
        probe_windows_scsi_flash(disk, db, &mut insp);
    }

    #[cfg(target_os = "macos")]
    {
        if insp.vid.is_some() {
            insp.probe_status = "已通过 macOS USB 描述符与 Chips.wdb 完成主控识别；受 macOS 内核 IOUSBMassStorageDriver 限制，用户态无法直发私有 SCSI 指令读取裸闪存 Flash ID（Windows 版支持自动 SCSI 穿透读取，macOS 下也可在下方输入已知 Flash ID 查询闪存颗粒）。".to_string();
        } else {
            insp.probe_status = "当前选中的不是 USB 移动存储设备（未获取到 USB VID/PID），无法匹配 U 盘主控；您可在下方直接搜索 Chips.wdb 离线数据库。".to_string();
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        insp.probe_status = "当前平台已启用 VID/PID 数据库匹配。".to_string();
    }

    insp
}

#[cfg(target_os = "windows")]
fn probe_windows_scsi_flash(disk: &DiskInfo, db: &ChipDatabase, insp: &mut ChipInspection) {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x0004D014;
    const SCSI_IOCTL_DATA_IN: u8 = 1;

    #[repr(C)]
    struct ScsiPassThroughDirectWithSense {
        length: u16,
        scsi_status: u8,
        path_id: u8,
        target_id: u8,
        lun: u8,
        cdb_length: u8,
        sense_info_length: u8,
        data_in: u8,
        _pad1: [u8; 3],
        data_transfer_length: u32,
        time_out_value: u32,
        data_buffer: *mut std::ffi::c_void,
        sense_info_offset: u32,
        cdb: [u8; 16],
        _pad2: [u8; 4],
        sense_buf: [u8; 32],
    }

    // Build volume device path "\\.\E:" from mount point "E:\"
    let mount_str = disk.mount_point.to_string_lossy();
    let dev_path = if mount_str.len() >= 2 && mount_str.as_bytes()[1] == b':' {
        format!(r"\\.\{}:", &mount_str[..1])
    } else if let Some(ref raw) = disk.raw_disk_path {
        raw.clone()
    } else {
        insp.probe_status = "无法定位设备节点，跳过底层 SCSI 闪存探测。".to_string();
        return;
    };

    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&dev_path)
    {
        Ok(f) => f,
        Err(_) => {
            insp.probe_status =
                "未获取到设备 SCSI 穿透权限（请以管理员身份运行本软件以直读主控固件与闪存 Flash ID）。"
                    .to_string();
            return;
        }
    };
    let handle = file.as_raw_handle();

    let send_cdb = |cdb: &[u8], out_len: usize| -> Option<Vec<u8>> {
        let mut data = vec![0u8; out_len];
        let mut spt = ScsiPassThroughDirectWithSense {
            length: (std::mem::size_of::<ScsiPassThroughDirectWithSense>() - 36) as u16,
            scsi_status: 0,
            path_id: 0,
            target_id: 0,
            lun: 0,
            cdb_length: cdb.len().min(16) as u8,
            sense_info_length: 32,
            data_in: SCSI_IOCTL_DATA_IN,
            _pad1: [0; 3],
            data_transfer_length: out_len as u32,
            time_out_value: 3,
            data_buffer: data.as_mut_ptr() as *mut std::ffi::c_void,
            sense_info_offset: (std::mem::size_of::<ScsiPassThroughDirectWithSense>() - 32) as u32,
            cdb: [0; 16],
            _pad2: [0; 4],
            sense_buf: [0; 32],
        };
        for (i, &b) in cdb.iter().take(16).enumerate() {
            spt.cdb[i] = b;
        }
        let mut bytes_ret: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_SCSI_PASS_THROUGH_DIRECT,
                &mut spt as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<ScsiPassThroughDirectWithSense>() as u32,
                &mut spt as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<ScsiPassThroughDirectWithSense>() as u32,
                &mut bytes_ret,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 && spt.scsi_status == 0 {
            Some(data)
        } else {
            None
        }
    };

    // 1. Standard SCSI INQUIRY (0x12) — safe on every USB/SATA/NVMe device!
    if let Some(inq) = send_cdb(&[0x12, 0x00, 0x00, 0x00, 0x60, 0x00], 96) {
        if inq.len() >= 36 {
            let v = String::from_utf8_lossy(&inq[8..16]).trim().to_string();
            let p = String::from_utf8_lossy(&inq[16..32]).trim().to_string();
            let r = String::from_utf8_lossy(&inq[32..36]).trim().to_string();
            if !v.is_empty() {
                insp.scsi_vendor = Some(v);
            }
            if !p.is_empty() {
                insp.scsi_product = Some(p);
            }
            if !r.is_empty() {
                insp.scsi_rev = Some(r);
            }
        }
    }

    // 2. Vendor-specific read-only Flash ID / Firmware Inquiry CDBs (only for removable/USB drives!)
    if disk.kind.is_external_or_removable() {
        let vendor_hint = insp
            .controller_match
            .as_ref()
            .map(|m| m.primary.chip_vendor.to_ascii_lowercase())
            .unwrap_or_default();

        // Try controller-matched probe first, then safe fallback probes for top 5 USB flash controllers
        let mut flash_bytes: Option<[u8; 6]> = None;

        // (a) Phison (群联): 0x06 0x56 Read Flash ID + 0x06 0x05 Get Info
        if vendor_hint.contains("phison") || vendor_hint.is_empty() {
            if let Some(buf) = send_cdb(&[0x06, 0x56, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 512) {
                if let Some(fid) = extract_valid_flash_id(&buf[..16]) {
                    flash_bytes = Some(fid);
                }
            }
            if let Some(info) = send_cdb(&[0x06, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 512) {
                if let Some(chip_str) = extract_ascii_chip_hint(&info, &["PS22", "PS23", "2251", "2307", "2309"]) {
                    insp.firmware_controller_hint = Some(chip_str);
                }
            }
        }

        // (b) Silicon Motion (慧荣 SMI): 0xF0 0x20 Read Flash ID + 0xF0 0x06 Read Info
        if flash_bytes.is_none() && (vendor_hint.contains("silicon") || vendor_hint.contains("smi") || vendor_hint.is_empty()) {
            if let Some(buf) = send_cdb(&[0xF0, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01], 512) {
                if let Some(fid) = extract_valid_flash_id(&buf[..16]) {
                    flash_bytes = Some(fid);
                }
            }
            if let Some(info) = send_cdb(&[0xF0, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01], 512) {
                if let Some(chip_str) = extract_ascii_chip_hint(&info, &["SM32", "SM23", "3267", "3268", "3281", "3282"]) {
                    insp.firmware_controller_hint = Some(chip_str);
                }
            }
        }

        // (c) Alcor (安国): 0x06 0x00 / 0x06 0xF0 Read Flash ID
        if flash_bytes.is_none() && vendor_hint.contains("alcor") {
            if let Some(buf) = send_cdb(&[0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 512) {
                if let Some(fid) = extract_valid_flash_id(&buf[..32]) {
                    flash_bytes = Some(fid);
                }
            }
        }

        // (d) Chipsbank (芯邦): 0xFA 0x06 Read Flash ID
        if flash_bytes.is_none() && vendor_hint.contains("chipsbank") {
            if let Some(buf) = send_cdb(&[0xFA, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 512) {
                if let Some(fid) = extract_valid_flash_id(&buf[..32]) {
                    flash_bytes = Some(fid);
                }
            }
        }

        // (e) Innostor (银灿): 0xF0 0x04 Read Flash ID
        if flash_bytes.is_none() && vendor_hint.contains("innostor") {
            if let Some(buf) = send_cdb(&[0xF0, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 512) {
                if let Some(fid) = extract_valid_flash_id(&buf[..32]) {
                    flash_bytes = Some(fid);
                }
            }
        }

        if let Some(fid) = flash_bytes {
            let hex = format!(
                "{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                fid[0], fid[1], fid[2], fid[3], fid[4], fid[5]
            );
            insp.flash_record = db.lookup_flash_id(&hex);
            if insp.flash_record.is_none() {
                insp.flash_vendor_fallback = Some(nand_manufacturer_name(fid[0]).to_string());
            }
            insp.flash_id_hex = Some(hex);
            insp.probe_status = "已通过 Windows 底层 SCSI 穿透成功读取物理闪存 Flash ID！".to_string();
        } else {
            insp.probe_status =
                "已完成 VID/PID 主控识别与标准 SCSI 查询；当前主控未响应公开 Flash ID 指令（常见于原厂加密主控如闪迪/三星或桥接盒）。"
                    .to_string();
        }
    } else {
        insp.probe_status = "非可移动 USB 闪存设备，已跳过 U 盘主控私有指令探测。".to_string();
    }
    drop(file);
}

#[cfg(target_os = "windows")]
fn extract_valid_flash_id(slice: &[u8]) -> Option<[u8; 6]> {
    if slice.len() < 6 {
        return None;
    }
    for win in slice.windows(6) {
        if is_known_nand_vendor(win[0])
            && win[1] != 0x00
            && win[1] != 0xFF
            && !win.iter().all(|&b| b == win[0])
            && (win[2] != 0x00 || win[3] != 0x00)
        {
            return Some([win[0], win[1], win[2], win[3], win[4], win[5]]);
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn extract_ascii_chip_hint(buf: &[u8], prefixes: &[&str]) -> Option<String> {
    let text = String::from_utf8_lossy(buf);
    for p in prefixes {
        if let Some(pos) = text.find(p) {
            let token: String = text[pos..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .take(20)
                .collect();
            if token.len() >= 4 {
                return Some(token);
            }
        }
    }
    None
}
