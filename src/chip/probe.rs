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
    /// True on Windows when opening the volume for SCSI pass-through failed due to missing admin rights
    pub needs_windows_admin: bool,
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
/// 2) On Windows: active read-only `IOCTL_SCSI_PASS_THROUGH_DIRECT` + `IOCTL_SCSI_PASS_THROUGH`
///    implementing all 10 ChipGenius controller family CDB suites
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
        needs_windows_admin: false,
    };

    #[cfg(target_os = "windows")]
    {
        probe_windows_scsi_flash(disk, db, &mut insp);
        if insp.controller_match.is_none()
            || insp
                .controller_match
                .as_ref()
                .map(|m| m.is_wildcard)
                .unwrap_or(false)
        {
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
            insp.probe_status = "已通过 macOS IOKit USB 描述符、SCSI 固件签名与 Chips.wdb 完成主控识别。受 macOS 内核 IOUSBMassStorageDriver 架构限制，用户态无法向 U 盘发送厂商私有 SCSI 指令读取裸闪存 Flash ID（Windows 版支持与 ChipGenius 相同的 10 大主控系 SCSI 穿透直读裸闪存）。".to_string();
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

/// Decodes Phison 16-bit big-endian chip ID found at byte offset `0x17A` of Phison `0x06 0x05` (528B) GetInfo response.
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

/// Scans a controller SCSI response payload for an exact 4-byte Flash ID match in `Chips.wdb`,
/// returning the 6-byte Flash ID, matched `FlashRecord`, and active CE count.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn find_wdb_flash_in_payload(
    buf: &[u8],
    db: &ChipDatabase,
    preferred_offsets: &[usize],
) -> Option<([u8; 6], FlashRecord, usize)> {
    // 1. Check preferred controller-specific offsets first (fastest & supports multi-CE counting)
    for &off in preferred_offsets {
        if off + 6 <= buf.len() {
            if let Some(rec) = db.lookup_exact_4bytes(&buf[off..off + 4]) {
                let fid = [
                    buf[off],
                    buf[off + 1],
                    buf[off + 2],
                    buf[off + 3],
                    buf[off + 4],
                    buf[off + 5],
                ];
                let ces = count_matching_ce_slots(buf, off, &fid[..4]);
                return Some((fid, rec, ces));
            }
        }
    }

    // 2. Scan whole payload at 1-byte granularity against the 1,136 exact 4-byte IDs in Chips.wdb
    let limit = buf.len().saturating_sub(5);
    for i in 0..limit {
        if let Some(rec) = db.lookup_exact_4bytes(&buf[i..i + 4]) {
            // Filter out accidental ASCII text like "2020" (0x32 0x30 ...) — known NAND IDs never start with ASCII digits
            let b0 = buf[i];
            if matches!(b0, 0x30..=0x39) {
                continue;
            }
            let fid = [
                buf[i],
                buf[i + 1],
                buf[i + 2],
                buf[i + 3],
                buf[i + 4],
                buf[i + 5],
            ];
            let ces = count_matching_ce_slots(buf, i, &fid[..4]);
            return Some((fid, rec, ces));
        }
    }
    None
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn count_matching_ce_slots(buf: &[u8], base_off: usize, id4: &[u8]) -> usize {
    for stride in [8usize, 16usize, 6usize, 32usize] {
        let mut count = 0usize;
        let mut pos = base_off;
        while pos + 4 <= buf.len() && count < 16 {
            if &buf[pos..pos + 4] == id4 {
                count += 1;
                pos += stride;
            } else {
                break;
            }
        }
        if count >= 2 {
            return count;
        }
    }
    1
}

#[cfg(target_os = "windows")]
fn probe_windows_scsi_flash(disk: &DiskInfo, db: &ChipDatabase, insp: &mut ChipInspection) {
    use crate::disk::direct_io::AlignedBuffer;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const IOCTL_SCSI_PASS_THROUGH: u32 = 0x0004D004;
    const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x0004D014;
    const SCSI_IOCTL_DATA_IN: u8 = 1;

    /// Exact Windows SDK `SCSI_PASS_THROUGH_DIRECT` (56 bytes on 64-bit, 44 bytes on 32-bit).
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

    /// Exact Windows SDK `SCSI_PASS_THROUGH` (buffered IOCTL 0x4D004 — used by 59 of 84 ChipGenius calls!).
    #[repr(C)]
    struct ScsiPassThrough {
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
        data_buffer_offset: usize,
        sense_info_offset: u32,
        cdb: [u8; 16],
    }

    #[repr(C)]
    struct ScsiPassThroughBuffered {
        spt: ScsiPassThrough,
        sense_buf: [u8; 32],
        data_buf: [u8; 2048],
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
        insp.needs_windows_admin = true;
        insp.probe_trace
            .push(format!("无法打开设备句柄进行 SCSI 穿透: {}", last_err));
        insp.probe_status =
            "未获取到设备 SCSI 穿透权限（请点击下方「以管理员身份重启」按钮，或右键以管理员身份运行本软件以直读主控与闪存 Flash ID）。"
                .to_string();
        return;
    };
    insp.probe_trace
        .push(format!("已打开底层 SCSI 设备句柄: {}", opened_path));
    let handle = file.as_raw_handle();

    // Helper: sends via buffered IOCTL_SCSI_PASS_THROUGH (0x4D004) or DIRECT (0x4D014), with automatic fallback!
    let send_cdb = |cdb: &[u8], out_len: usize, prefer_buffered: bool| -> Result<Vec<u8>, u32> {
        let try_direct = || -> Result<Vec<u8>, u32> {
            let alloc_len = ((out_len.max(16) + 4095) / 4096) * 4096;
            let mut aligned = AlignedBuffer::new(alloc_len);
            let sense_off =
                std::mem::offset_of!(ScsiPassThroughDirectWithSense, sense_buf) as u32;
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
                Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(req.sptd.scsi_status as i32) as u32)
            }
        };

        let try_buffered = || -> Result<Vec<u8>, u32> {
            if out_len > 2048 {
                return try_direct();
            }
            let sense_off = std::mem::offset_of!(ScsiPassThroughBuffered, sense_buf) as u32;
            let data_off = std::mem::offset_of!(ScsiPassThroughBuffered, data_buf);
            let mut req = ScsiPassThroughBuffered {
                spt: ScsiPassThrough {
                    length: std::mem::size_of::<ScsiPassThrough>() as u16,
                    scsi_status: 0,
                    path_id: 0,
                    target_id: 0,
                    lun: 0,
                    cdb_length: cdb.len().min(16) as u8,
                    sense_info_length: 32,
                    data_in: SCSI_IOCTL_DATA_IN,
                    data_transfer_length: out_len as u32,
                    time_out_value: 4,
                    data_buffer_offset: data_off,
                    sense_info_offset: sense_off,
                    cdb: [0; 16],
                },
                sense_buf: [0; 32],
                data_buf: [0; 2048],
            };
            for (i, &b) in cdb.iter().take(16).enumerate() {
                req.spt.cdb[i] = b;
            }
            let total_size = (data_off + out_len) as u32;
            let mut bytes_ret: u32 = 0;
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    IOCTL_SCSI_PASS_THROUGH,
                    &mut req as *mut _ as *mut std::ffi::c_void,
                    total_size,
                    &mut req as *mut _ as *mut std::ffi::c_void,
                    total_size,
                    &mut bytes_ret,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 && req.spt.scsi_status == 0 {
                Ok(req.data_buf[..out_len].to_vec())
            } else {
                Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(req.spt.scsi_status as i32) as u32)
            }
        };

        if prefer_buffered {
            try_buffered().or_else(|_| try_direct())
        } else {
            try_direct().or_else(|_| try_buffered())
        }
    };

    // 1. Standard SCSI INQUIRY (0x12)
    match send_cdb(&[0x12, 0x00, 0x00, 0x00, 0x60, 0x00], 96, true) {
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
                .push(format!("SCSI INQUIRY (0x12) 失败 (code={})", code));
        }
    }

    // Only skip vendor probes on the OS boot/system drive; allow any non-system drive (even if bus was detected as Unknown/Fixed)
    if disk.is_system {
        insp.probe_status = "系统启动盘跳过 U 盘主控私有指令探测。".to_string();
        drop(file);
        return;
    }

    // Helper to record a decoded Flash ID from any controller probe
    let mut found_flash: Option<([u8; 6], Option<FlashRecord>, usize)> = None;
    let mut try_capture_flash = |label: &str,
                                 buf: &[u8],
                                 preferred_offsets: &[usize],
                                 vendor_name: &str,
                                 insp: &mut ChipInspection|
     -> bool {
        if found_flash.is_some() {
            return true;
        }
        // Ignore all-zero or all-0xFF dummy buffers
        if buf.iter().all(|&b| b == 0x00) || buf.iter().all(|&b| b == 0xFF) {
            return false;
        }
        // 1st priority: exact 4-byte match in Chips.wdb (virtually 0% false positive!)
        if let Some((fid, rec, ces)) = find_wdb_flash_in_payload(buf, db, preferred_offsets) {
            insp.probe_trace.push(format!(
                "{} 命中 Chips.wdb 闪存: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} [{}CE] -> {} {}",
                label, fid[0], fid[1], fid[2], fid[3], fid[4], fid[5], ces, rec.manufacturer, rec.flash_type
            ));
            found_flash = Some((fid, Some(rec), ces));
            if insp.firmware_vendor_hint.is_none() && !vendor_name.is_empty() {
                insp.firmware_vendor_hint = Some(vendor_name.to_string());
            }
            return true;
        }
        // 2nd priority: check preferred controller offsets for newer JEDEC NAND Flash ID not yet in 2019 Chips.wdb
        for &off in preferred_offsets {
            if off + 6 <= buf.len() && is_valid_6byte_flash_id(&buf[off..off + 6]) {
                let fid = [
                    buf[off],
                    buf[off + 1],
                    buf[off + 2],
                    buf[off + 3],
                    buf[off + 4],
                    buf[off + 5],
                ];
                let ces = count_matching_ce_slots(buf, off, &fid[..4]);
                insp.probe_trace.push(format!(
                    "{} 读到裸 Flash ID (偏移 0x{:X}): {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} [{}CE]",
                    label, off, fid[0], fid[1], fid[2], fid[3], fid[4], fid[5], ces
                ));
                found_flash = Some((fid, None, ces));
                if insp.firmware_vendor_hint.is_none() && !vendor_name.is_empty() {
                    insp.firmware_vendor_hint = Some(vendor_name.to_string());
                }
                return true;
            }
        }
        // Log non-empty response prefix for diagnostics
        let hex_head: String = buf
            .iter()
            .take(16)
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ");
        insp.probe_trace
            .push(format!("{} 返回 {}B (前16B: {})", label, buf.len(), hex_head));
        false
    };

    // =========================================================================
    // Exact ChipGenius v4.21 SCSI CDB Suites (all 10 major controller families)
    // =========================================================================

    // ---- 1. Silicon Motion (慧荣 SMI: SM325x / SM326x / SM327x / SM328x / SM2320) ----
    // Exact ChipGenius 16-byte CDBs: F0 06, F0 20 (... 01 07), F0 2A, F0 04!
    let smi_probes: &[(&str, [u8; 16], usize)] = &[
        ("SMI F0 06 (SM326/327/328)", [0xF0, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0], 512),
        ("SMI F0 20 (FlashID)",       [0xF0, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x07, 0, 0, 0], 512),
        ("SMI F0 2A (SM3257EN)",      [0xF0, 0x2A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0], 512),
        ("SMI F0 04 (SM325)",         [0xF0, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0], 512),
    ];
    for &(label, cdb, len) in smi_probes {
        if let Ok(buf) = send_cdb(&cdb, len, false) {
            if let Some(chip_str) = extract_ascii_chip_hint(
                &buf,
                &["SM32", "SM23", "SM22", "3255", "3257", "3267", "3268", "3269", "3271", "3280", "3281", "3282"],
            ) {
                insp.firmware_vendor_hint = Some("Silicon Motion (慧荣)".to_string());
                insp.firmware_controller_hint = Some(chip_str);
            }
            if try_capture_flash(label, &buf, &[0, 8, 16, 32, 64], "Silicon Motion (慧荣)", insp) {
                break;
            }
        }
    }

    // ---- 2. Phison (群联: PS2251-03/-07/-09/-11/-17/-19/-67/-68/-70) ----
    // Exact ChipGenius CDBs: 06 05 (len=12, xfer=528!), 06 56 (len=12, xfer=512)!
    if found_flash.is_none() || insp.firmware_controller_hint.is_none() {
        if let Ok(info) = send_cdb(&[0x06, 0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 528, false) {
            if info.len() >= 0x17C {
                let chip_id = u16::from_be_bytes([info[0x17A], info[0x17B]]);
                if let Some(model) = decode_phison_chip_id(chip_id) {
                    insp.probe_trace.push(format!(
                        "Phison 06 05 (528B) 命中芯片 ID 0x{:04X} -> {}",
                        chip_id, model
                    ));
                    insp.firmware_vendor_hint = Some("Phison (群联)".to_string());
                    insp.firmware_controller_hint = Some(model.to_string());
                    if let Some(fw) = extract_phison_fw_version(&info) {
                        insp.firmware_version_hint = Some(fw);
                    }
                }
            }
            let _ = try_capture_flash("Phison 06 05", &info, &[0, 0x10, 0x80], "Phison (群联)", insp);
        }
        if found_flash.is_none() {
            if let Ok(buf) = send_cdb(&[0x06, 0x56, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 512, true) {
                let _ = try_capture_flash("Phison 06 56", &buf, &[0, 16, 32], "Phison (群联)", insp);
            }
        }
    }

    // ---- 3. Chipsbank (芯邦: CBM2090..CBM2199E/S) ----
    // Exact ChipGenius CDBs: EA 01 ... E3 (16B, xfer=1536), E6 00 (12B, xfer=128), E3 01 (12B, xfer=128)!
    if found_flash.is_none() {
        let cbm_probes: &[(&str, &[u8], usize)] = &[
            ("Chipsbank EA 01", &[0xEA, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xE3], 1536),
            ("Chipsbank E6 00", &[0xE6, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 128),
            ("Chipsbank E3 01", &[0xE3, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 128),
        ];
        for &(label, cdb, len) in cbm_probes {
            if let Ok(buf) = send_cdb(cdb, len, true) {
                if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["CBM20", "CBM21", "CBM11", "2098", "2099", "2199"]) {
                    insp.firmware_vendor_hint = Some("Chipsbank (芯邦)".to_string());
                    insp.firmware_controller_hint = Some(chip_str);
                }
                if try_capture_flash(label, &buf, &[0, 8, 16, 32, 64, 512], "Chipsbank (芯邦)", insp) {
                    break;
                }
            }
        }
    }

    // ---- 4. Alcor Micro (安国: AU6981..AU6989SN / SN-GT / SNL / AU6998) ----
    // Exact ChipGenius CDBs: FA 0E (8B, xfer=512), FA 00 (8B, xfer=512)!
    if found_flash.is_none() {
        let alcor_probes: &[(&str, &[u8], usize)] = &[
            ("Alcor FA 0E", &[0xFA, 0x0E, 0, 0, 0, 0, 0, 0], 512),
            ("Alcor FA 00", &[0xFA, 0x00, 0, 0, 0, 0, 0, 0], 512),
        ];
        for &(label, cdb, len) in alcor_probes {
            if let Ok(buf) = send_cdb(cdb, len, false) {
                if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["AU69", "AU64", "SC90", "SC70"]) {
                    insp.firmware_vendor_hint = Some("Alcor Micro (安国)".to_string());
                    insp.firmware_controller_hint = Some(chip_str);
                }
                if try_capture_flash(label, &buf, &[0, 8, 16, 32], "Alcor Micro (安国)", insp) {
                    break;
                }
            }
        }
    }

    // ---- 5. Innostor (银灿: IS902 / IS903 / IS916 / IS917 / IS918 / IS918M) ----
    // Exact ChipGenius CDBs: FC ... "INQU" (16B, 128B), FC ... "NAND" (16B, 512B), E0 10 06 90 ... 8A A3 (IS918M), CA 00 (IS918M_GA)!
    if found_flash.is_none() {
        let inno_inqu = [0xFC, 0x00, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, b'I', b'N', b'Q', b'U'];
        if let Ok(buf) = send_cdb(&inno_inqu, 128, true) {
            if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["IS90", "IS91", "IS92"]) {
                insp.probe_trace.push(format!("Innostor FC INQU 命中主控: {}", chip_str));
                insp.firmware_vendor_hint = Some("Innostor (银灿)".to_string());
                insp.firmware_controller_hint = Some(chip_str);
            }
        }
        let inno_probes: &[(&str, &[u8], usize, bool)] = &[
            ("Innostor FC NAND", &[0xFC, 0x00, 0x01, 0, 0, 0, 0, 0, 0x02, 0, 0, 0x05, b'N', b'A', b'N', b'D'], 512, false),
            ("Innostor IS918M E0", &[0xE0, 0x10, 0x06, 0x90, 0x00, 0x0E, 0, 0, 0x01, 0, 0x8A, 0xA3], 512, true),
            ("Innostor/ASolid CA 00", &[0xCA, 0x00, 0, 0, 0, 0, 0x06, 0, 0, 0, 0, 0], 512, true),
        ];
        for &(label, cdb, len, buffered) in inno_probes {
            if let Ok(buf) = send_cdb(cdb, len, buffered) {
                if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["IS90", "IS91", "IS92", "AS32"]) {
                    insp.firmware_vendor_hint = Some("Innostor (银灿)".to_string());
                    insp.firmware_controller_hint = Some(chip_str);
                }
                if try_capture_flash(label, &buf, &[0, 8, 16, 32], "Innostor (银灿)", insp) {
                    break;
                }
            }
        }
    }

    // ---- 6. FirstChip (一芯: FC1178 / FC1178BC / FC1178E / FC1179) ----
    // Exact ChipGenius CDBs: F1 01 ... A0 (16B, 512B), F1 00 ... A1 (16B, 512B)!
    if found_flash.is_none() {
        let fc_probes: &[(&str, [u8; 16])] = &[
            ("FirstChip F1 01 A0", [0xF1, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xA0]),
            ("FirstChip F1 00 A1", [0xF1, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xA1]),
        ];
        for &(label, cdb) in fc_probes {
            if let Ok(buf) = send_cdb(&cdb, 512, false) {
                if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["FC11", "FC13", "FC83"]) {
                    insp.firmware_vendor_hint = Some("FirstChip (一芯)".to_string());
                    insp.firmware_controller_hint = Some(chip_str);
                }
                if try_capture_flash(label, &buf, &[0, 8, 16, 32, 64], "FirstChip (一芯)", insp) {
                    break;
                }
            }
        }
    }

    // ---- 7. Solid State System (鑫创 SSS: SSS6673..SSS6698 / TC58NC66xx) ----
    // Exact ChipGenius CDBs: FF 02 (12B, 16B), FF 00 68 (12B, 512B)!
    if found_flash.is_none() {
        if let Ok(buf) = send_cdb(&[0xFF, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16, true) {
            let _ = try_capture_flash("SSS FF 02", &buf, &[0, 4, 8], "Solid State System (鑫创)", insp);
        }
        if found_flash.is_none() {
            if let Ok(buf) = send_cdb(&[0xFF, 0x00, 0x68, 0, 0, 0, 0, 0, 0, 0, 0, 0], 512, true) {
                let _ = try_capture_flash("SSS FF 00 68", &buf, &[0, 8, 16], "Solid State System (鑫创)", insp);
            }
        }
    }

    // ---- 8. Yeestor / SiliconGo / DM (得一微 / 硅格 / 点序: SG1580 / SG1581 / DM8233 / DM8235) ----
    // Exact ChipGenius CDBs: A1 00 (16B, 36B), A5 00 00 00 01 (12B, 32B)!
    if found_flash.is_none() {
        if let Ok(buf) = send_cdb(&[0xA1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 36, false) {
            if let Some(chip_str) = extract_ascii_chip_hint(&buf, &["SG15", "DM82", "YS90", "SiliconGo"]) {
                insp.firmware_vendor_hint = Some("Yeestor / SiliconGo (得一微/硅格)".to_string());
                insp.firmware_controller_hint = Some(chip_str);
            }
        }
        if let Ok(buf) = send_cdb(&[0xA5, 0x00, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0], 32, false) {
            let _ = try_capture_flash("Yeestor/SG A5", &buf, &[0, 8, 16], "Yeestor / SiliconGo (得一微/硅格)", insp);
        }
    }

    // ---- 9. Micov (迈科微: MW8209 / MW8229 / MW8259 / MW8269 / MW8289) ----
    // Exact ChipGenius CDBs: D3 00 (16B, 512B), D6 00 90 08 00 00 01 (16B, 16B)!
    if found_flash.is_none() {
        if let Ok(buf) = send_cdb(&[0xD6, 0x00, 0x90, 0x08, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16, true) {
            let _ = try_capture_flash("Micov D6 00 90", &buf, &[0, 4, 8], "Micov (迈科微)", insp);
        }
        if found_flash.is_none() {
            if let Ok(buf) = send_cdb(&[0xD3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 512, true) {
                let _ = try_capture_flash("Micov D3 00", &buf, &[0, 16, 32], "Micov (迈科微)", insp);
            }
        }
    }

    // ---- 10. ITE / Usbest (联阳: UT165 / UT167 / IT1167 / IT1170) + DF/F5 Probes ----
    // Exact ChipGenius CDBs: FD 80 ... 06 ... 01 (16B, 64B), F2 00 (12B, 34B), F5 01 (12B, 24B), DF 00 ... D3 (16B, 512B)!
    if found_flash.is_none() {
        let extra_probes: &[(&str, &[u8], usize, &str)] = &[
            ("ITE FD 80", &[0xFD, 0x80, 0, 0, 0, 0, 0, 0, 0x06, 0, 0, 0, 0, 0x01, 0, 0], 64, "ITE / Usbest (联阳)"),
            ("ITE F2 00", &[0xF2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 34, "ITE / Usbest (联阳)"),
            ("Vendor F5 01", &[0xF5, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 24, ""),
            ("Vendor DF D3", &[0xDF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xD3], 512, ""),
        ];
        for &(label, cdb, len, vname) in extra_probes {
            if let Ok(buf) = send_cdb(cdb, len, true) {
                if try_capture_flash(label, &buf, &[0, 8, 16], vname, insp) {
                    break;
                }
            }
        }
    }

    // ---- 11. SAT-16 ATA IDENTIFY DEVICE (USB-SATA / USB-NVMe Bridges & SSD Flash Drives) ----
    let sat16_identify = [
        0x85, 0x08, 0x0E, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xEC, 0x00,
    ];
    if let Ok(ata) = send_cdb(&sat16_identify, 512, false) {
        if let Some((model, fw)) = parse_ata_identify_model_fw(&ata) {
            insp.probe_trace
                .push(format!("SAT-16 ATA IDENTIFY 成功: {} (FW {})", model, fw));
            insp.bridge_ata_model = Some(format!("{} (FW {})", model, fw));
        }
    }

    if let Some((fid, rec_opt, ces)) = found_flash {
        let hex = format!(
            "{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
            fid[0], fid[1], fid[2], fid[3], fid[4], fid[5]
        );
        insp.flash_record = rec_opt.or_else(|| db.lookup_flash_id(&hex));
        if insp.flash_record.is_none() {
            insp.flash_vendor_fallback = Some(nand_manufacturer_name(fid[0]).to_string());
        }
        insp.flash_id_hex = Some(hex);
        insp.flash_ce_count = Some(ces);
        insp.probe_status =
            "已通过 Windows 底层 SCSI 穿透成功直读主控与物理闪存 Flash ID！".to_string();
    } else if insp.firmware_controller_hint.is_some() {
        insp.probe_status =
            "已通过底层 SCSI 穿透直读到主控型号，但该固件未返回可识别的裸闪存 Flash ID（可展开下方「底层探测诊断明细」查看原始返回字节）。"
                .to_string();
    } else if insp.bridge_ata_model.is_some() {
        insp.probe_status =
            "已通过 SAT-16 穿透读取到 USB 桥接内置固态硬盘型号与固件版本！".to_string();
    } else {
        insp.probe_status =
            "已完成全部 10 大主控系 SCSI 穿透探测；当前设备未响应公开私有指令（常见于原厂加密主控，可展开下方「底层探测诊断明细」反馈原始返回信息）。"
                .to_string();
    }

    drop(file);
}

#[cfg(target_os = "windows")]
fn is_valid_6byte_flash_id(win: &[u8]) -> bool {
    win.len() >= 6
        && is_known_nand_vendor(win[0])
        && win[1] != 0x00
        && win[1] != 0xFF
        && !win.iter().all(|&b| b == win[0])
        && (win[2] != 0x00 || win[3] != 0x00)
        && !(win[0] == 0x20 && win[1] == 0x20)
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
    fn payload_wdb_flash_scanner_finds_flash_id_and_counts_ce() {
        let db = ChipDatabase::load_default();
        // Simulate a 512-byte controller reply with a 2CE Toshiba TLC-16K Flash ID (98 3A 98 B3 76 72) at offset 16 & 32
        let mut payload = vec![0u8; 512];
        payload[16..22].copy_from_slice(&[0x98, 0x3A, 0x98, 0xB3, 0x76, 0x72]);
        payload[32..38].copy_from_slice(&[0x98, 0x3A, 0x98, 0xB3, 0x76, 0x72]);

        let (fid, rec, ces) =
            find_wdb_flash_in_payload(&payload, &db, &[0, 16]).expect("found flash ID");
        assert_eq!(fid, [0x98, 0x3A, 0x98, 0xB3, 0x76, 0x72]);
        assert_eq!(rec.flash_type, "TLC-16K");
        assert_eq!(ces, 2);
    }

    #[test]
    fn scsi_heuristic_identifies_oem_rebranded_controllers() {
        let db = ChipDatabase::load_default();
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

        let abcd = db
            .lookup_controller("0xABCD", "0x1234", None)
            .expect("ABCD:1234 match");
        assert!(abcd.primary.note.contains("扩容"));
    }
}
