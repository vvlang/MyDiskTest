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
    /// Exact controller vendor & model read directly from controller firmware via vendor SCSI CDB (Windows)
    pub firmware_vendor_hint: Option<String>,
    pub firmware_controller_hint: Option<String>,
    pub firmware_version_hint: Option<String>,
    /// Internal SSD model/firmware read via SAT-16 ATA IDENTIFY through USB bridge (Windows)
    pub bridge_ata_model: Option<String>,
    /// Raw NAND Flash ID bytes in uppercase spaced hex (e.g. "98 3A 98 B3 76 72")
    pub flash_id_hex: Option<String>,
    /// Active CE channel count detected from multi-CE Flash ID response (e.g. 1, 2, 4)
    pub flash_ce_count: Option<usize>,
    /// Decoded NAND flash record matched from Chips.wdb
    pub flash_record: Option<FlashRecord>,
    /// Fallback NAND manufacturer if byte 0 is recognized but exact ID is newer than Chips.wdb
    pub flash_vendor_fallback: Option<String>,
    /// Human-readable status summary
    pub probe_status: String,
    /// Diagnostic trace of every hardware probe step attempted
    pub probe_trace: Vec<String>,
}

impl ChipInspection {
    /// Generates a classic ChipGenius-style copyable Chinese hardware report.
    pub fn format_chipgenius_report(&self, disk: &DiskInfo) -> String {
        let mut s = String::with_capacity(640);
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

        let eff_vendor = self
            .firmware_vendor_hint
            .as_deref()
            .or_else(|| self.controller_match.as_ref().map(|m| m.primary.chip_vendor.as_str()))
            .filter(|v| !v.is_empty());
        let eff_ctrl = self
            .firmware_controller_hint
            .as_deref()
            .filter(|c| !c.is_empty())
            .or_else(|| {
                self.controller_match
                    .as_ref()
                    .map(|m| m.primary.controller.as_str())
                    .filter(|c| !c.is_empty())
            });

        if let Some(v) = eff_vendor {
            s.push_str(&format!("芯片厂商: {}\n", v));
        }
        if let Some(c) = eff_ctrl {
            s.push_str(&format!("芯片型号: {}\n", c));
        } else {
            s.push_str("芯片型号: 未识别（可能是内置盘或未收录的新款主控）\n");
        }
        if let Some(ref fw) = self.firmware_version_hint {
            s.push_str(&format!("主控固件: {}\n", fw));
        }
        if let Some(ref ata) = self.bridge_ata_model {
            s.push_str(&format!("桥接内置盘: {}\n", ata));
        }
        if let Some(ref m) = self.controller_match {
            if !m.primary.note.is_empty() {
                s.push_str(&format!("数据库备注: {}\n", m.primary.note));
            }
            if !m.primary.url.is_empty() {
                s.push_str(&format!("参考资料: {}\n", m.primary.url));
            }
        }

        if let Some(ref fid) = self.flash_id_hex {
            let ce = self
                .flash_ce_count
                .map(|n| format!(" ({}CE)", n))
                .unwrap_or_default();
            s.push_str(&format!("闪存识别码: {}{}\n", fid, ce));
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

/// Performs USB controller & NAND flash inspection for the selected drive using:
/// 1) `Chips.wdb` + expanded 2020-2026 controller VID/PID + SCSI Inquiry signature engine
/// 2) On Windows: active read-only `IOCTL_SCSI_PASS_THROUGH_DIRECT` multi-controller firmware & Flash ID probing
/// 3) On macOS: live IOKit `IOUSBHostDevice` + `IOSCSIPeripheralDevice` descriptors & signature inference
pub fn probe_disk_chip(disk: &DiskInfo, db: &ChipDatabase) -> ChipInspection {
    let vid = disk.vid.clone();
    let pid = disk.pid.clone();
    let bcd = disk.bcd_device.clone();

    let controller_match = db.lookup_controller_with_scsi(
        vid.as_deref().unwrap_or(""),
        pid.as_deref().unwrap_or(""),
        bcd.as_deref(),
        disk.scsi_vendor.as_deref(),
        disk.scsi_product.as_deref(),
        disk.scsi_rev.as_deref(),
    );

    let mut insp = ChipInspection {
        vid,
        pid,
        bcd_device: bcd,
        scsi_vendor: disk.scsi_vendor.clone(),
        scsi_product: disk.scsi_product.clone(),
        scsi_rev: disk.scsi_rev.clone(),
        controller_match,
        firmware_vendor_hint: None,
        firmware_controller_hint: None,
        firmware_version_hint: None,
        bridge_ata_model: None,
        flash_id_hex: None,
        flash_ce_count: None,
        flash_record: None,
        flash_vendor_fallback: None,
        probe_status: String::new(),
        probe_trace: Vec::new(),
    };

    #[cfg(target_os = "windows")]
    {
        probe_windows_scsi_flash(disk, db, &mut insp);
        // Re-run controller lookup if live SCSI Inquiry gave us fresh vendor/product/rev!
        if insp.controller_match.is_none() || insp.controller_match.as_ref().map(|m| m.is_wildcard).unwrap_or(false) {
            if let Some(better) = db.lookup_controller_with_scsi(
                insp.vid.as_deref().unwrap_or(""),
                insp.pid.as_deref().unwrap_or(""),
                insp.bcd_device.as_deref(),
                insp.scsi_vendor.as_deref(),
                insp.scsi_product.as_deref(),
                insp.scsi_rev.as_deref(),
            ) {
                insp.controller_match = Some(better);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let (Some(ref v), Some(ref p)) = (&insp.vid, &insp.pid) {
            insp.probe_trace.push(format!(
                "IOKit USB 描述符: VID={} PID={} bcdDevice={}",
                v,
                p,
                insp.bcd_device.as_deref().unwrap_or("-")
            ));
        }
        if let (Some(ref sv), Some(ref sp)) = (&insp.scsi_vendor, &insp.scsi_product) {
            insp.probe_trace.push(format!(
                "IOKit SCSI Inquiry: {} / {} (Rev {})",
                sv,
                sp,
                insp.scsi_rev.as_deref().unwrap_or("-")
            ));
        }
        if insp.vid.is_some() || insp.controller_match.is_some() {
            insp.probe_status = "已通过 macOS IOKit USB 描述符、SCSI 固件签名与 Chips.wdb 完成主控识别。受 macOS 内核 IOUSBMassStorageDriver 架构限制，用户态无法向 U 盘发送厂商私有 SCSI 指令读取裸闪存 Flash ID（如需直读裸闪存颗粒 ID，请在 Windows 版右键以管理员身份运行）。".to_string();
        } else {
            insp.probe_status = "当前选中的不是 USB 移动存储设备（未获取到 USB VID/PID），无法匹配 U 盘主控；您可在下方直接搜索 Chips.wdb 离线数据库。".to_string();
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        insp.probe_status = "当前平台已启用 VID/PID 与 SCSI 特征匹配。".to_string();
    }

    insp
}

/// Decodes Phison 16-bit big-endian chip ID found at byte offset `0x17A` of Phison `0x06 0x05` GetInfo response.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn decode_phison_chip_id(id: u16) -> Option<&'static str> {
    match id {
        0x2250 => Some("PS2251-50 (2250)"),
        0x2260 => Some("PS2251-60 (2260)"),
        0x2261 => Some("PS2251-61 (2261)"),
        0x2267 => Some("PS2251-67 (2267)"),
        0x2268 => Some("PS2251-68 (2268)"),
        0x2270 => Some("PS2251-70 (2270)"),
        0x2301 => Some("PS2251-01 (2301)"),
        0x2302 => Some("PS2251-02 (2302)"),
        0x2303 => Some("PS2251-03 (2303)"),
        0x2307 => Some("PS2251-07 (2307)"),
        0x2308 => Some("PS2251-08 (2308)"),
        0x2309 => Some("PS2251-09 (2309)"),
        0x2311 => Some("PS2251-11 (2311)"),
        0x2317 => Some("PS2251-17 (2317)"),
        0x2319 => Some("PS2251-19 (2319)"),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn probe_windows_scsi_flash(disk: &DiskInfo, db: &ChipDatabase, insp: &mut ChipInspection) {
    use crate::disk::direct_io::AlignedBuffer;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x0004D014;
    const SCSI_IOCTL_DATA_IN: u8 = 1;

    /// Exact Windows SDK `SCSI_PASS_THROUGH_DIRECT` layout:
    /// `size_of == 56` on 64-bit Windows (`x86_64` / `aarch64`), `44` on 32-bit Windows.
    #[repr(C)]
    struct ScsiPassThroughDirect {
        length: u16,
        scsi_status: u8,
        path_id: u8,
        target_id: u8,
        lun: u8,
        cdb_length: u8,
        sense_info_length: u8,
        data_in: u8,
        data_transfer_length: u32,
        time_out_value: u32,
        data_buffer: *mut std::ffi::c_void,
        sense_info_offset: u32,
        cdb: [u8; 16],
    }

    #[repr(C)]
    struct ScsiPassThroughDirectWithSense {
        sptd: ScsiPassThroughDirect,
        sense_buf: [u8; 32],
    }

    // Try opening both logical volume `\\.\E:` and physical drive `\\.\PhysicalDriveN`
    let mut path_candidates = Vec::new();
    let mount_str = disk.mount_point.to_string_lossy();
    if mount_str.len() >= 2 && mount_str.as_bytes()[1] == b':' {
        path_candidates.push(format!(r"\\.\{}:", &mount_str[..1]));
    }
    if let Some(ref raw) = disk.raw_disk_path {
        path_candidates.push(raw.clone());
    }

    let mut opened: Option<(std::fs::File, String)> = None;
    let mut last_err = String::new();
    for p in &path_candidates {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(p)
        {
            Ok(f) => {
                opened = Some((f, p.clone()));
                break;
            }
            Err(e) => {
                last_err = format!("{} ({})", p, e);
            }
        }
    }

    let Some((file, opened_path)) = opened else {
        insp.probe_trace
            .push(format!("无法打开设备句柄进行 SCSI 穿透: {}", last_err));
        insp.probe_status =
            "未获取到设备 SCSI 穿透权限（请右键「以管理员身份运行」本软件以直读主控芯片型号与闪存 Flash ID）。"
                .to_string();
        return;
    };
    insp.probe_trace
        .push(format!("已打开底层 SCSI 设备句柄: {}", opened_path));
    let handle = file.as_raw_handle();

    let send_cdb = |cdb: &[u8], out_len: usize| -> Result<Vec<u8>, u32> {
        let alloc_len = ((out_len + 4095) / 4096) * 4096;
        let mut aligned = AlignedBuffer::new(alloc_len);
        let sense_off = std::mem::offset_of!(ScsiPassThroughDirectWithSense, sense_buf) as u32;
        let mut req = ScsiPassThroughDirectWithSense {
            sptd: ScsiPassThroughDirect {
                length: std::mem::size_of::<ScsiPassThroughDirect>() as u16,
                scsi_status: 0,
                path_id: 0,
                target_id: 0,
                lun: 0,
                cdb_length: cdb.len().min(16) as u8,
                sense_info_length: 32,
                data_in: SCSI_IOCTL_DATA_IN,
                data_transfer_length: out_len as u32,
                time_out_value: 4,
                data_buffer: aligned.as_mut_slice().as_mut_ptr() as *mut std::ffi::c_void,
                sense_info_offset: sense_off,
                cdb: [0; 16],
            },
            sense_buf: [0; 32],
        };
        for (i, &b) in cdb.iter().take(16).enumerate() {
            req.sptd.cdb[i] = b;
        }
        let mut bytes_ret: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_SCSI_PASS_THROUGH_DIRECT,
                &mut req as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<ScsiPassThroughDirectWithSense>() as u32,
                &mut req as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<ScsiPassThroughDirectWithSense>() as u32,
                &mut bytes_ret,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 && req.sptd.scsi_status == 0 {
            Ok(aligned.as_slice()[..out_len].to_vec())
        } else {
            let err_code = std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(req.sptd.scsi_status as i32) as u32;
            Err(err_code)
        }
    };

    // 1. Standard SCSI INQUIRY (0x12) — works on virtually every USB/SATA/NVMe disk
    match send_cdb(&[0x12, 0x00, 0x00, 0x00, 0x60, 0x00], 96) {
        Ok(inq) if inq.len() >= 36 => {
            let v = String::from_utf8_lossy(&inq[8..16]).trim().to_string();
            let p = String::from_utf8_lossy(&inq[16..32]).trim().to_string();
            let r = String::from_utf8_lossy(&inq[32..36]).trim().to_string();
            insp.probe_trace
                .push(format!("SCSI INQUIRY (0x12) 成功: {} / {} (Rev {})", v, p, r));
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
        Ok(_) => {}
        Err(code) => {
            insp.probe_trace
                .push(format!("SCSI INQUIRY (0x12) 未返回数据 (code={})", code));
        }
    }

    if !disk.kind.is_external_or_removable() {
        insp.probe_status = "非可移动 USB 存储设备，已跳过 U 盘主控私有指令探测。".to_string();
        drop(file);
        return;
    }

    // 2. Active ChipGenius-style multi-controller read-only vendor probes (NOT gated on VID!)
    let mut flash_bytes: Option<[u8; 6]> = None;
    let mut ce_count: Option<usize> = None;

    // ---- Family A: Silicon Motion (慧荣 SMI) ----
    if let Ok(info) = send_cdb(&[0xF0, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01], 512) {
        if let Some(chip_str) = extract_ascii_chip_hint(
            &info,
            &["SM32", "SM23", "SM22", "3255", "3257", "3267", "3268", "3271", "3280", "3281", "3282"],
        ) {
            insp.probe_trace
                .push(format!("SMI (0xF0 0x06) 命中主控固件: {}", chip_str));
            insp.firmware_vendor_hint = Some("Silicon Motion (慧荣)".to_string());
            insp.firmware_controller_hint = Some(chip_str);
            if let Some(isp) = extract_ascii_chip_hint(&info, &["ISP ", "ISP-", "V1.", "V2."]) {
                insp.firmware_version_hint = Some(isp);
            }
        }
    }
    for (label, cdb) in [
        ("SMI 0xF0 0x20", [0xF0, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]),
        ("SMI 0xF0 0x0A", [0xF0, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]),
    ] {
        if flash_bytes.is_none() {
            if let Ok(buf) = send_cdb(&cdb, 512) {
                if let Some((fid, ces)) = extract_multi_ce_flash_id(&buf, 8) {
                    insp.probe_trace.push(format!("{} 成功读取 Flash ID ({}CE)", label, ces));
                    flash_bytes = Some(fid);
                    ce_count = Some(ces);
                    if insp.firmware_vendor_hint.is_none() {
                        insp.firmware_vendor_hint = Some("Silicon Motion (慧荣)".to_string());
                    }
                }
            }
        }
    }

    // ---- Family B: Phison (群联) ----
    if insp.firmware_controller_hint.is_none() || flash_bytes.is_none() {
        if let Ok(info) = send_cdb(&[0x06, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 512) {
            if info.len() >= 0x17C {
                let chip_id = u16::from_be_bytes([info[0x17A], info[0x17B]]);
                if let Some(model) = decode_phison_chip_id(chip_id) {
                    insp.probe_trace
                        .push(format!("Phison (0x06 0x05) 命中主控芯片 ID 0x{:04X} -> {}", chip_id, model));
                    insp.firmware_vendor_hint = Some("Phison (群联)".to_string());
                    insp.firmware_controller_hint = Some(model.to_string());
                    // Extract Phison firmware version bytes (offset 0x94..0x9C or ASCII)
                    if let Some(fw) = extract_phison_fw_version(&info) {
                        insp.firmware_version_hint = Some(fw);
                    }
                } else if let Some(chip_str) = extract_ascii_chip_hint(&info, &["PS22", "PS23", "FC11", "FC13"]) {
                    insp.probe_trace.push(format!("0x06 0x05 命中主控标识: {}", chip_str));
                    insp.firmware_controller_hint = Some(chip_str);
                }
            }
        }
        for (label, cdb) in [
            ("Phison 0x06 0x56", [0x06, 0x56, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            ("Phison 0x06 0x06", [0x06, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
        ] {
            if flash_bytes.is_none() {
                if let Ok(buf) = send_cdb(&cdb, 512) {
                    if let Some((fid, ces)) = extract_multi_ce_flash_id(&buf, 16) {
                        insp.probe_trace.push(format!("{} 成功读取 Flash ID ({}CE)", label, ces));
                        flash_bytes = Some(fid);
                        ce_count = Some(ces);
                        if insp.firmware_vendor_hint.is_none() {
                            insp.firmware_vendor_hint = Some("Phison (群联)".to_string());
                        }
                    }
                }
            }
        }
    }

    // ---- Family C: Alcor Micro (安国) ----
    if flash_bytes.is_none() {
        for (label, cdb) in [
            ("Alcor 0x06 0x00", [0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            ("Alcor 0x06 0xF0", [0x06, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            ("Alcor 0x06 0x09", [0x06, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
        ] {
            if flash_bytes.is_none() {
                if let Ok(buf) = send_cdb(&cdb, 512) {
                    if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["AU69", "AU64", "SC90"]) {
                        insp.firmware_vendor_hint = Some("Alcor Micro (安国)".to_string());
                        insp.firmware_controller_hint = Some(chip_str);
                    }
                    if let Some(fid) = extract_valid_flash_id(&buf[..64]) {
                        insp.probe_trace.push(format!("{} 成功读取 Flash ID", label));
                        flash_bytes = Some(fid);
                        if insp.firmware_vendor_hint.is_none() {
                            insp.firmware_vendor_hint = Some("Alcor Micro (安国)".to_string());
                        }
                    }
                }
            }
        }
    }

    // ---- Family D: Chipsbank (芯邦) ----
    if flash_bytes.is_none() {
        for (label, cdb) in [
            ("Chipsbank 0xFA 0x06", [0xFA, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            ("Chipsbank 0xFA 0x00", [0xFA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
        ] {
            if flash_bytes.is_none() {
                if let Ok(buf) = send_cdb(&cdb, 512) {
                    if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["CBM20", "CBM21", "CBM11"]) {
                        insp.firmware_vendor_hint = Some("Chipsbank (芯邦)".to_string());
                        insp.firmware_controller_hint = Some(chip_str);
                    }
                    if let Some(fid) = extract_valid_flash_id(&buf[..64]) {
                        insp.probe_trace.push(format!("{} 成功读取 Flash ID", label));
                        flash_bytes = Some(fid);
                        if insp.firmware_vendor_hint.is_none() {
                            insp.firmware_vendor_hint = Some("Chipsbank (芯邦)".to_string());
                        }
                    }
                }
            }
        }
    }

    // ---- Family E: Innostor (银灿) ----
    if flash_bytes.is_none() {
        for (label, cdb) in [
            ("Innostor 0xF0 0x04", [0xF0, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            ("Innostor 0xF0 0x01", [0xF0, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
        ] {
            if flash_bytes.is_none() {
                if let Ok(buf) = send_cdb(&cdb, 512) {
                    if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["IS90", "IS91", "IS92"]) {
                        insp.firmware_vendor_hint = Some("Innostor (银灿)".to_string());
                        insp.firmware_controller_hint = Some(chip_str);
                    }
                    if let Some(fid) = extract_valid_flash_id(&buf[..64]) {
                        insp.probe_trace.push(format!("{} 成功读取 Flash ID", label));
                        flash_bytes = Some(fid);
                        if insp.firmware_vendor_hint.is_none() {
                            insp.firmware_vendor_hint = Some("Innostor (银灿)".to_string());
                        }
                    }
                }
            }
        }
    }

    // ---- Family F: FirstChip (一芯) ----
    if flash_bytes.is_none() {
        if let Ok(buf) = send_cdb(&[0xFF, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 512) {
            if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["FC11", "FC13", "FC83"]) {
                insp.firmware_vendor_hint = Some("FirstChip (一芯)".to_string());
                insp.firmware_controller_hint = Some(chip_str);
            }
            if let Some(fid) = extract_valid_flash_id(&buf[..64]) {
                insp.probe_trace.push("FirstChip (0xFF 0x01) 成功读取 Flash ID".to_string());
                flash_bytes = Some(fid);
            }
        }
    }

    // ---- Family G: SAT-16 ATA IDENTIFY DEVICE (USB-SATA / USB-NVMe Bridges & SSD Flash Drives) ----
    let sat16_identify = [
        0x85, 0x08, 0x0E, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xEC, 0x00,
    ];
    if let Ok(ata) = send_cdb(&sat16_identify, 512) {
        if let Some((model, fw)) = parse_ata_identify_model_fw(&ata) {
            insp.probe_trace
                .push(format!("SAT-16 ATA IDENTIFY 成功: {} (FW {})", model, fw));
            insp.bridge_ata_model = Some(format!("{} (FW {})", model, fw));
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
        insp.flash_ce_count = ce_count;
        insp.probe_status = "已通过 Windows 底层 SCSI 穿透成功直读主控固件与物理闪存 Flash ID！".to_string();
    } else if insp.firmware_controller_hint.is_some() {
        insp.probe_status = "已通过底层 SCSI 穿透直读到主控型号，但该固件未开放裸闪存 Flash ID 读取。".to_string();
    } else if insp.bridge_ata_model.is_some() {
        insp.probe_status = "已通过 SAT-16 穿透读取到 USB 桥接内置固态硬盘型号与固件版本！".to_string();
    } else {
        insp.probe_status =
            "已完成 VID/PID、SCSI Inquiry 与全系主控探测；当前设备未响应公开私有指令（常见于闪迪/三星原厂加密主控或 NVMe 桥接盒）。"
                .to_string();
    }

    drop(file);
}

#[cfg(target_os = "windows")]
fn extract_multi_ce_flash_id(buf: &[u8], stride: usize) -> Option<([u8; 6], usize)> {
    let mut first: Option<[u8; 6]> = None;
    let mut ce_count = 0usize;
    let max_slots = (buf.len() / stride).min(16);
    for slot in 0..max_slots {
        let base = slot * stride;
        if base + 6 > buf.len() {
            break;
        }
        let s = &buf[base..base + 6];
        if is_valid_6byte_flash_id(s) {
            if first.is_none() {
                first = Some([s[0], s[1], s[2], s[3], s[4], s[5]]);
            }
            ce_count += 1;
        }
    }
    if let Some(f) = first {
        return Some((f, ce_count.max(1)));
    }
    // Fallback sliding scan in first 64 bytes
    extract_valid_flash_id(&buf[..buf.len().min(64)]).map(|f| (f, 1))
}

#[cfg(target_os = "windows")]
fn is_valid_6byte_flash_id(win: &[u8]) -> bool {
    win.len() >= 6
        && is_known_nand_vendor(win[0])
        && win[1] != 0x00
        && win[1] != 0xFF
        && !win.iter().all(|&b| b == win[0])
        && (win[2] != 0x00 || win[3] != 0x00)
        && !(win[0] == 0x20 && win[1] == 0x20) // ignore ASCII spaces "  "
}

#[cfg(target_os = "windows")]
fn extract_valid_flash_id(slice: &[u8]) -> Option<[u8; 6]> {
    if slice.len() < 6 {
        return None;
    }
    for win in slice.windows(6) {
        if is_valid_6byte_flash_id(win) {
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
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
                .take(24)
                .collect();
            if token.len() >= 4 {
                return Some(token);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn extract_phison_fw_version(info: &[u8]) -> Option<String> {
    if info.len() >= 0x9B && info[0x94] < 0x30 && info[0x95] < 0x64 {
        // Phison binary firmware version at 0x94..0x96 or ASCII around 0x94
        let v = format!("{:02X}.{:02X}.{:02X}", info[0x94], info[0x95], info[0x96]);
        if v != "00.00.00" && v != "FF.FF.FF" {
            return Some(v);
        }
    }
    None
}

/// Decodes ATA IDENTIFY DEVICE 512-byte response: words 27..46 (Model) and words 23..26 (Firmware).
#[cfg(target_os = "windows")]
fn parse_ata_identify_model_fw(ata: &[u8]) -> Option<(String, String)> {
    if ata.len() < 512 {
        return None;
    }
    let decode_ata_str = |start_word: usize, word_count: usize| -> String {
        let mut bytes = Vec::with_capacity(word_count * 2);
        for w in start_word..start_word + word_count {
            let lo = ata[w * 2];
            let hi = ata[w * 2 + 1];
            bytes.push(hi);
            bytes.push(lo);
        }
        String::from_utf8_lossy(&bytes).trim().to_string()
    };
    let model = decode_ata_str(27, 20);
    let fw = decode_ata_str(23, 4);
    if model.len() >= 3 && model.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        Some((model, fw))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phison_chip_id_decoder_works() {
        assert_eq!(decode_phison_chip_id(0x2307), Some("PS2251-07 (2307)"));
        assert_eq!(decode_phison_chip_id(0x2319), Some("PS2251-19 (2319)"));
        assert_eq!(decode_phison_chip_id(0x0000), None);
    }

    #[test]
    fn scsi_heuristic_identifies_oem_rebranded_controllers() {
        let db = ChipDatabase::load_default();
        // Generic VID/PID + Alcor 8.07 SCSI signature
        let m = db
            .lookup_controller_with_scsi(
                "0x058F",
                "0x6387",
                Some("1.00"),
                Some("Generic"),
                Some("Flash Disk"),
                Some("8.07"),
            )
            .expect("Alcor match");
        assert!(m.primary.chip_vendor.contains("Alcor"));

        // Unknown/spoofed VID + PMAP Phison signature
        let p = db
            .lookup_controller_with_scsi(
                "0xFFFF",
                "0x9999",
                None,
                Some("USB"),
                Some("DISK 3.0"),
                Some("PMAP"),
            )
            .expect("Phison PMAP heuristic");
        assert!(p.primary.chip_vendor.contains("Phison"));

        // ABCD:1234 Chipsbank/FirstChip classic fake/generic VID:PID
        let abcd = db
            .lookup_controller("0xABCD", "0x1234", None)
            .expect("ABCD:1234 match");
        assert!(abcd.primary.note.contains("扩容"));
    }
}
