#[cfg(target_os = "windows")]
use crate::chip::wdb::{decode_flash_id_algorithmic, is_known_nand_vendor, nand_manufacturer_name};
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
    /// Dual-channel CE Flash ID mapping table (`[通道 0]`, `[通道 1]`, up to 8 slots each)
    pub flash_channel_map: Option<(Vec<Option<String>>, Vec<Option<String>>)>,
    /// Decoded NAND flash record matched from Chips.wdb or algorithmic JEDEC decoder
    pub flash_record: Option<FlashRecord>,
    /// Fallback NAND manufacturer if byte 0 is recognized
    pub flash_vendor_fallback: Option<String>,
    /// Human-readable status summary
    pub probe_status: String,
    /// Diagnostic trace of every hardware probe step attempted
    pub probe_trace: Vec<String>,
    /// True on Windows when opening the volume for SCSI pass-through failed due to missing admin rights
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub needs_windows_admin: bool,
    /// Set on Windows when an Innostor IS902~IS917 controller requires an external RAM applet for Flash ID
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub innostor_deep_probe_model: Option<String>,
}

impl ChipInspection {
    /// Generates a classic ChipGenius-style copyable Chinese hardware report.
    pub fn format_chipgenius_report(&self, disk: &DiskInfo) -> String {
        let mut s = String::with_capacity(640);
        s.push_str(&format!("设备描述: {}\n", disk.name));
        s.push_str(&format!("设备类型: {}\n", disk.kind.label_zh()));
        s.push_str(&format!("协议版本: {}\n", disk.protocol));
        if let Some(ref spd) = disk.link_speed {
            s.push_str(&format!("当前速度: {}\n", spd));
        }
        if let Some(ma) = disk.max_power_ma {
            s.push_str(&format!("电力消耗: {}mA\n", ma));
        }
        s.push_str(&format!("挂载路径: {}\n", disk.mount_point.display()));
        if let (Some(ref vid), Some(ref pid)) = (&self.vid, &self.pid) {
            s.push_str(&format!("设备 ID: VID = {}  PID = {}\n", vid, pid));
        }
        if let Some(ref bcd) = self.bcd_device {
            s.push_str(&format!("设备修订版: {}\n", bcd));
        }
        if let Some(ref um) = disk.usb_manufacturer {
            s.push_str(&format!("设备供应商: {}\n", um));
        } else if !disk.vendor.is_empty() {
            s.push_str(&format!("设备厂商: {}\n", disk.vendor));
        }
        if let Some(ref up) = disk.usb_product {
            s.push_str(&format!("设备名称: {}\n", up));
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

        if let Some((ref ch0, ref ch1)) = self.flash_channel_map {
            s.push_str("\n闪存识别码映射表\n------------------\n[通道 0]\t[通道 1]\n");
            let rows = ch0.len().max(ch1.len());
            for i in 0..rows {
                let c0 = ch0.get(i).and_then(|x| x.as_deref()).unwrap_or("------------");
                let c1 = ch1.get(i).and_then(|x| x.as_deref()).unwrap_or("------------");
                s.push_str(&format!("{}\t{}\n", c0, c1));
            }
        }

        s
    }
}

/// Performs USB controller & NAND flash inspection for the selected drive using:
/// 1) `Chips.wdb` + expanded 2020-2026 controller VID/PID + SCSI Inquiry signature engine
/// 2) On Windows: full ChipGenius v4.21 17-family SCSI Pass-Through engine (buffered 0x4D004 + direct 0x4D014
///    + no-data wakeup commands + exact payload offset/chip-code decoders + multi-CE Flash ID extraction)
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
        flash_channel_map: None,
        flash_record: None,
        flash_vendor_fallback: None,
        probe_status: String::new(),
        probe_trace: Vec::new(),
        needs_windows_admin: false,
        innostor_deep_probe_model: None,
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
            insp.probe_status = "已通过 macOS IOKit USB 描述符、SCSI 固件签名与 Chips.wdb 完成主控识别。受 macOS 内核 IOUSBMassStorageDriver 架构限制，用户态无法向 U 盘发送厂商私有 SCSI 指令读取裸闪存 Flash ID（Windows 版已完整内置 ChipGenius v4.21 全部 17 大主控家族 SCSI 穿透直读引擎）。".to_string();
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

// =============================================================================
// Cross-platform Controller Firmware Response Parsers (Reverse-Engineered from ChipGenius v4.21)
// =============================================================================

/// Parses Phison `0x06 0x05` (528-byte GetInfo response) using exact ChipGenius v4.21 offsets:
/// - Chip ID bytes at `0x17E..0x17F` (fallback `0x17A..0x17B`): formats `PS2251-{lo:02X} (PS{hi:02X}{lo:02X})`
/// - Firmware version at `0x94..0x96`, build date at `0x97..0x99`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_phison_0605_info(info: &[u8]) -> Option<(String, Option<String>)> {
    if info.len() < 0x180 {
        return None;
    }
    let (hi, lo) = if matches!(info[0x17E], 0x22 | 0x23 | 0x31 | 0x50) && info[0x17F] != 0x00 && info[0x17F] != 0xFF {
        (info[0x17E], info[0x17F])
    } else if matches!(info[0x17A], 0x22 | 0x23 | 0x31 | 0x50) && info[0x17B] != 0x00 && info[0x17B] != 0xFF {
        (info[0x17A], info[0x17B])
    } else {
        return None;
    };

    let model = format!("PS2251-{:02X} (PS{:02X}{:02X})", lo, hi, lo);
    let fw = if info[0x94] < 0x30
        && info[0x95] < 0x99
        && (info[0x94] != 0 || info[0x95] != 0 || info[0x96] != 0)
    {
        let ver = format!("{:02X}.{:02X}.{:02X}", info[0x94], info[0x95], info[0x96]);
        let y = info[0x97];
        let m = info[0x98];
        let d = info[0x99];
        if (1..=35).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d) {
            Some(format!("{} [20{:02}-{:02}-{:02}]", ver, y, m, d))
        } else {
            Some(ver)
        }
    } else {
        None
    };

    Some((model, fw))
}

/// Exact 48-entry Alcor Micro chip code table reverse-engineered from ChipGenius v4.21 (`0x004503D5`),
/// decoded from `u16::from_be_bytes([buf[4], buf[5]])` of the 1-byte `0x9A` SCSI response.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn decode_alcor_chip_code(code: u16) -> Option<&'static str> {
    let exact = match code {
        0xA101 => Some("AU3151"),
        0xAF05 => Some("AU6384"),
        0xAC43 => Some("AU9387"),
        0xBA01 => Some("AU6983/FC8308"),
        0xBB09 => Some("AU6984"),
        0xBC01 | 0xBC07 => Some("AU6986"),
        0xBD01 => Some("AU6988"),
        0xBD02 => Some("AU6988/AU6986"),
        0xBD05 => Some("AU6984/AU6988"),
        0xF401 => Some("AU3150"),
        0xF402 => Some("AU3150C"),
        0xCA07 | 0xCA09 => Some("SC708AN/AU6987AN"),
        0xD203 | 0xD403 => Some("SC508B(FC8508B)/AU6985B"),
        0x7662 => Some("AU6998N"),
        0xCC01 => Some("SC908NL/AU6989NL"),
        0xCD03 | 0xE001 => Some("SC908/AU6989"),
        0x7660 => Some("SC908L/AU6989L"),
        0xCF02 => Some("SC908(N)L/AU6989(N)L"),
        0xCA08 | 0xE306 => Some("SC908AN/AU6989AN"),
        0xE302 | 0xE304 => Some("SC908AN(L)/AU6989AN(L)"),
        0xE50B => Some("AU6989SN/AU6989ANL"),
        0xE505 | 0xE509 | 0xE50E | 0xE512 | 0xE802 | 0xEB02 => Some("SC908SN/AU6989SN"),
        0xEC05 | 0xEC07 => Some("AU6989SN-GT"),
        0xEF00 => Some("AU6989SNL"),
        0xEF01 => Some("AU6989SN-GTA"),
        0xF101 => Some("AU6989SNL-B"),
        0xF204 | 0xF206 => Some("AU6989SN-GTB"),
        0xF500 | 0xF502 => Some("AU6989SN-GTC"),
        0xF700 => Some("AU6989SN-GTD"),
        0xF800 | 0xF880 | 0xF8C0 => Some("AU6989SN-GTE"),
        0xF980 | 0xFA80 => Some("AU6989SN-GTF"),
        _ => None,
    };
    if exact.is_some() {
        return exact;
    }
    match code & 0xFF00 {
        0xE600 => Some("SC908 <Special>"),
        0xE400 => Some("SC908N <Special>"),
        _ => None,
    }
}

/// Parses Silicon Motion `F0 04` / `F0 2A` 512-byte response where exact controller model ASCII
/// resides at `0x190..0x1AE` (e.g. `SM3267AB`, `SM3268AB`, `SM3271AB`, `SM3280BB`, `SM3281BB`).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_smi_model_block(buf: &[u8]) -> Option<String> {
    if buf.len() >= 0x1AF {
        let slice = &buf[0x190..0x1AF];
        let s: String = slice
            .iter()
            .take_while(|&&b| b.is_ascii_graphic() || b == b' ')
            .map(|&b| b as char)
            .collect::<String>()
            .trim()
            .to_string();
        if s.starts_with("SM") && s.len() >= 5 {
            return Some(s);
        }
    }
    extract_ascii_chip_hint(
        buf,
        &[
            "SM3282", "SM3281", "SM3280", "SM3271", "SM3269", "SM3268", "SM3267", "SM3261",
            "SM3257", "SM3255", "SM3254", "SM325", "SM2320", "SM23", "SM22",
        ],
    )
}

/// Parses FirstChip `F1 01 ... A0` response (exact ChipGenius v4.21 `0x004BD8AE` logic):
/// verifies magic `[0x1A, 0x2B, 0x3C, 0x4D]` at `0x20..0x24` and firmware string/year.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_firstchip_f101(buf: &[u8]) -> Option<String> {
    if buf.len() < 0x40 {
        return None;
    }
    let has_magic = buf[0x20..0x24] == [0x1A, 0x2B, 0x3C, 0x4D];
    let text = String::from_utf8_lossy(buf);
    let lower = text.to_ascii_lowercase();
    if !has_magic && !lower.contains("chip") && !lower.contains("fc11") && !lower.contains("zc32") {
        return None;
    }
    if let Some(exact) = extract_ascii_chip_hint(buf, &["FC1178", "FC1179", "FC1308", "ZC3270"]) {
        return Some(exact);
    }
    if lower.contains("zerochip") || text.contains("2018") {
        Some("ZC3270".to_string())
    } else if text.contains("2016") {
        Some("FC1178E".to_string())
    } else if text.contains("2017") {
        Some("FC1178BC".to_string())
    } else {
        Some("FC1179".to_string())
    }
}

/// Exact 19-entry Solid State System (SSS / Toshiba TC58NC66xx) chip code table
/// reverse-engineered from ChipGenius v4.21 (`0x0047C760`), decoded from `buf[0]` of `FF 02`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn decode_sss_chip_code(code: u8) -> Option<&'static str> {
    match code {
        0x71 => Some("SSS6673"),
        0x87 => Some("SSS6675"),
        0x88 => Some("TC58NC6686/SSS6675"),
        0x91 => Some("SSS6675/SSS6677A"),
        0xA1 => Some("TC58NC6687/SSS6677"),
        0xA7 => Some("SSS6677"),
        0xB0 => Some("TC58NC6688/SSS6690"),
        0xB1 => Some("SSS6690"),
        0xB2 => Some("TC58NC6689/SSS6691"),
        0xB3 => Some("SSS6691"),
        0xB4 => Some("TC58NC6690/SSS6692"),
        0xB5 => Some("TC58NC6621/SSS6692"),
        0xB6 => Some("TC58NC6611/SSS6696"),
        0xB7 => Some("SSS6697"),
        0xB9 => Some("SSS6696"),
        0xBA => Some("TC58NC6623/SSS6698"),
        0xBB => Some("SSS6698"),
        0xBD => Some("SSS6695"),
        0xC0 => Some("SSS6679"),
        _ => None,
    }
}

/// Parses Skymedi `CA` (10-byte CDB, 512-byte response) using exact ChipGenius v4.21 (`0x0046ECAC`)
/// big-endian 32-bit chip signature at `buf[0..4]` (plus `buf[8]` for `SK6226AA`/`SK6226BA`).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_skymedi_ca(buf: &[u8]) -> Option<String> {
    if buf.len() >= 4 {
        let sig = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let exact = match sig {
            0x16030801 => Some("SK6281AA"),
            0x16031109 => Some("SK6281AB"),
            0x28060100 => Some("SK6211AA"),
            0x28060200 => Some("SK6211BA"),
            0x28060300 => Some("SK6211BB"),
            0x28070100 => Some("SK6213AA/SK6215"),
            0x28070200 => Some("SK6215AA/SK6213"),
            0x12261102 => {
                if buf.get(8).copied().unwrap_or(0) != 0 {
                    Some("SK6226BA")
                } else {
                    Some("SK6226AA")
                }
            }
            0x12210782 | 0x12210901 | 0x12211406 | 0x12211494 | 0x12211764 => Some("SK6221"),
            0x00101006 => Some("SK6216"),
            0x12270101 => Some("SK6236A"),
            0x12310806 | 0x12310832 | 0x12310874 => Some("SK6231"),
            _ => None,
        };
        if let Some(m) = exact {
            return Some(m.to_string());
        }
    }
    extract_ascii_chip_hint(buf, &["SK62", "SKYMEDI"])
}

/// Parses Chipsbank `EA 01 ... E3` / `E3 01` response using exact ChipGenius v4.21 (`0x004461A6`)
/// controller tags (`CHIP`, `2092..2199`, and firmware build-date disambiguation).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_chipsbank_info(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(buf);
    for &(needle, model) in &[
        ("2098EB", "CBM2098EB(V98EB)"),
        ("2098A", "CBM2098A(V98A)"),
        ("2098B", "CBM2098(V98)/CBM2098P(V98P)"),
        ("2098E", "CBM2098E(V98E)"),
        ("2098S", "CBM2098S(V98S)"),
        ("2096D", "CBM2096T(V96T)/CBM2096PT(V96PT)"),
        ("2096E", "CBM2096E(V96E)"),
        ("2095C", "CBM2096(V96)/CBM2096P(V96P)"),
        ("2095S", "CBM2096(V96)/CBM2096S(V96S)"),
        ("2095", "CBM2095(V89)/CBM2093P(V88P)"),
        ("2093P", "CBM2093(V88)"),
        ("2093E", "CBM2093E(V88E)"),
        ("2093V", "CBM2093(V88)"),
        ("2092X", "CBM2092(V68)"),
        ("2092", "CBM2092(V68)"),
        ("2099A", "CBM2099A"),
        ("2099E", "CBM2099E"),
        ("2099X", "CBM2099S"),
        ("2099S", "CBM2099S"),
        ("2199A", "CBM2199A"),
        ("2199S", "CBM2199S"),
        ("2199X", "CBM2199X"),
        ("2198A", "CBM2198A"),
        ("2198X", "CBM2198X"),
    ] {
        if text.contains(needle) {
            return Some(model.to_string());
        }
    }
    if text.contains("209XE") || text.contains("219XE") || text.contains("CHIP") {
        for &(date_tag, model) in &[
            ("2012  09  10", "CBM2098EB(V98EB)"),
            ("2012  12  20", "CBM2098E(V98E)"),
            ("2013  06  06", "CBM2098S(V98S)"),
            ("2013  10  15", "CBM2093E(V88E)"),
            ("2015  04  27", "CBM2099E"),
            ("2015  05  12", "CBM2099E"),
            ("2016  03  02", "CBM2099E"),
            ("2016  10  15", "CBM2099E"),
            ("2017  05  17", "CBM2199A"),
            ("2017  10  01", "CBM2199S"),
            ("2017  12  12", "CBM2199A"),
        ] {
            if text.contains(date_tag) {
                return Some(model.to_string());
            }
        }
    }
    extract_ascii_chip_hint(buf, &["CBM20", "CBM21", "CBM11"])
}

/// Parses ASolid / Innostor `CA 00 ... "AS"` (512-byte) signature response:
/// - `18002S...` (e.g. `18002SO3T_4A1002`) -> `IS918EN (ASolid 18002)` + F/W version
/// - `18002B...`                           -> `IS918EN (BootROM)` + F/W version
/// - `13008B...` / `13008S...`             -> `IS918M_GA (BootROM)` / `IS918M_GA`
/// - `13002B030_000001` / `13002B030_000004` / `13002...` -> `AS3257ENLT` / `AS3257H` / `AS3257`
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_asolid_is918_ca00(buf: &[u8]) -> Option<(String, String, Option<String>)> {
    if buf.len() < 6 {
        return None;
    }
    let sig: String = buf
        .iter()
        .take(32)
        .take_while(|&&b| b.is_ascii_graphic() || b == b' ')
        .map(|&b| b as char)
        .collect::<String>()
        .trim()
        .to_string();
    let text = String::from_utf8_lossy(buf);

    let fw = if sig.len() >= 6 && sig.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        Some(sig.clone())
    } else {
        None
    };

    if sig.starts_with("18002B") || text.contains("18002B") {
        return Some((
            "ASolid / Innostor (点序/银灿)".to_string(),
            "IS918EN (BootROM)".to_string(),
            fw,
        ));
    }
    if sig.starts_with("18002") || text.contains("18002S") || text.contains("18002") {
        return Some((
            "ASolid / Innostor (点序/银灿)".to_string(),
            "IS918EN (ASolid 18002)".to_string(),
            fw,
        ));
    }
    if sig.starts_with("13008B") || text.contains("13008B") {
        return Some((
            "Innostor / ASolid (银灿/点序)".to_string(),
            "IS918M_GA (BootROM)".to_string(),
            fw,
        ));
    }
    if sig.starts_with("13008") || text.contains("13008S") || text.contains("13008") {
        return Some((
            "Innostor / ASolid (银灿/点序)".to_string(),
            "IS918M_GA".to_string(),
            fw,
        ));
    }
    if text.contains("13002B030_000001") {
        return Some(("ASolid (点序)".to_string(), "AS3257ENLT".to_string(), fw));
    }
    if text.contains("13002B030_000004") {
        return Some(("ASolid (点序)".to_string(), "AS3257H".to_string(), fw));
    }
    if sig.starts_with("13002") || text.contains("13002") {
        return Some(("ASolid (点序)".to_string(), "AS3257".to_string(), fw));
    }
    // Generic modern ASolid / IS918 5-digit generation prefix (e.g. 1600x, 1900x, 2000x + B/S/M)
    let b = sig.as_bytes();
    if b.len() >= 6
        && matches!(b[0], b'1' | b'2')
        && b[1..5].iter().all(|c| c.is_ascii_digit())
        && matches!(b[5], b'S' | b'B' | b'M' | b'C')
    {
        return Some((
            "ASolid / Innostor (点序/银灿)".to_string(),
            format!("IS918 / ASolid ({})", &sig[..5]),
            fw,
        ));
    }
    None
}

/// Parses HY6919 `DF ... D3` (512-byte) response using ChipGenius v4.21 (`0x004C2CB6`)
/// status/version bytes at `buf[0x21..0x25]` (`0x51..0x54` minus `0x30` buffered SPT header).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_hy6919_d3(buf: &[u8]) -> Option<(String, Option<String>)> {
    if buf.len() < 0x25 {
        return None;
    }
    let text = String::from_utf8_lossy(buf);
    if text.contains("HY6919") || text.contains("6919") {
        let ver_word = u32::from_be_bytes([buf[0x21], buf[0x22], buf[0x23], buf[0x24]]);
        let fw = if ver_word != 0 && ver_word != 0xFFFF_FFFF {
            Some(format!(
                "{:02X}.{:02X}.{:02X}.{:02X}",
                buf[0x21], buf[0x22], buf[0x23], buf[0x24]
            ))
        } else {
            None
        };
        return Some(("HY6919".to_string(), fw));
    }
    None
}

/// Parses Yeestor / SiliconGo / DM `A1` (36-byte) controller signature response (ChipGenius `0x00483A1A`).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_yeestor_a1(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(buf);
    for &(needle, model) in &[
        ("SG1581", "SG1581 (DM8235)"),
        ("DM8235", "SG1581 (DM8235)"),
        ("SG1580", "SG1580 (DM8233)"),
        ("1580FLY", "SG1580 (DM8233)"),
        ("DM8233", "SG1580 (DM8233)"),
        ("SG5081", "SG5081"),
        ("YS8231", "YS8231 (T18E)"),
        ("YS908", "YS908x"),
        ("UD6816", "UD6816 (T18D/E)"),
        ("UD6810", "UD6810"),
        ("UD6809", "UD6809"),
        ("SGOFEZBB", "SG1580 / SG1581"),
        ("SGFREEDOM", "SG1580 / T20"),
        ("SiliconGo", "SiliconGo / Yeestor"),
    ] {
        if text.contains(needle) {
            return Some(model.to_string());
        }
    }
    extract_ascii_chip_hint(buf, &["SG15", "DM82", "YS90", "UD68"])
}

/// Deinterleaves 16-bit bus Micov `D6 00 90 08` response where even bytes `[0,2,4,6,8,10]`
/// and odd bytes `[1,3,5,7,9,11]` hold two separate 8-bit Flash ID channels.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn deinterleave_micov_d6(buf: &[u8]) -> Option<([u8; 6], [u8; 6])> {
    if buf.len() < 12 {
        return None;
    }
    let even = [buf[0], buf[2], buf[4], buf[6], buf[8], buf[10]];
    let odd = [buf[1], buf[3], buf[5], buf[7], buf[9], buf[11]];
    Some((even, odd))
}

/// Parses Innostor `FC ... "INQU"` (128-byte) response to extract full model + chip revision
/// (e.g., `IS902E-A1`, `IS903-A5`, `IS917-E1`) using ChipGenius `0x0045E094` (`buf[0]` hex rev).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_innostor_inqu(buf: &[u8]) -> Option<String> {
    let base = extract_ascii_chip_hint(
        buf,
        &[
            "IS902E", "IS902", "IS903", "IS916EN", "IS916", "IS917", "IS918M", "IS918",
        ],
    )?;
    // If model doesn't already have a `-XX` suffix and payload byte 0 holds chip stepping (e.g. 0xA1 -> "-A1")
    if !base.contains('-') && !buf.is_empty() && buf[0] != 0x00 && buf[0] != 0xFF {
        Some(format!("{}-{:02X}", base, buf[0]))
    } else {
        Some(base)
    }
}

/// Parses Innostor `FC <04|05> FE ... "NFTL"` (16-byte) response for 4-byte BCD/hex firmware version
/// (e.g. `[0x02, 0x07, 0x18, 0x00]` -> `"2.07.18.00"`).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_innostor_nftl_fw(buf: &[u8]) -> Option<String> {
    if buf.len() >= 4 && buf[0] > 0 && buf[0] < 0x30 && !(buf[1] == 0xFF && buf[2] == 0xFF) {
        Some(format!("{:X}.{:02X}.{:02X}.{:02X}", buf[0], buf[1], buf[2], buf[3]))
    } else {
        None
    }
}

/// Returns the exact ChipGenius v4.21 `(nftl_cdb, nand_cdb)` pair for a given Innostor controller model:
/// - `IS902 / IS902E / IS916`: `NFTL(CDB[1]=0x04)`, `NAND(CDB[1]=0x04, CDB[3]=0x02, CDB[10]=0x08)`
/// - `IS903`:                  `NFTL(CDB[1]=0x05)`, `NAND(CDB[1]=0x03, CDB[3]=0x02, CDB[10]=0x04)`
/// - `IS916EN / IS917`:        `NFTL(CDB[1]=0x05)`, `NAND(CDB[1]=0x03, CDB[3]=0x00, CDB[10]=0x04)`
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn innostor_cdbs_for_model(model: &str) -> ([u8; 16], [u8; 16]) {
    let up = model.to_ascii_uppercase();
    let is_916en_or_917 = up.contains("IS916EN") || up.contains("IS917");
    let is_903 = up.contains("IS903");
    let is_new_gen = is_903 || is_916en_or_917;

    let nftl_b1 = if is_new_gen { 0x05 } else { 0x04 };
    let nand_b1 = if is_new_gen { 0x03 } else { 0x04 };
    let nand_b3 = if is_916en_or_917 { 0x00 } else { 0x02 };
    let nand_b10 = if is_new_gen { 0x04 } else { 0x08 };

    let nftl = [
        0xFC, nftl_b1, 0xFE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, b'N', b'F',
        b'T', b'L',
    ];
    let nand = [
        0xFC, nand_b1, 0x01, nand_b3, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, nand_b10, 0x05, b'N',
        b'A', b'N', b'D',
    ];
    (nftl, nand)
}

/// Returns `(sector_count, ledc_cdb, exit_tepf_cdb)` for Innostor RAM-mode deep probe & clean exit.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn innostor_ram_mode_params(model: &str) -> (u8, [u8; 16], [u8; 16]) {
    let up = model.to_ascii_uppercase();
    let is_916en_or_917 = up.contains("IS916EN") || up.contains("IS917");
    let is_903 = up.contains("IS903");
    let is_new_gen = is_903 || is_916en_or_917;

    let sectors: u8 = if is_new_gen {
        0x60 // 48 KiB (IS903 / IS916EN / IS917)
    } else if up.contains("IS902E") || up.contains("IS916") {
        0x40 // 32 KiB (IS902E / IS916)
    } else {
        0x20 // 16 KiB (IS902)
    };

    let ledc_b10 = if is_916en_or_917 { 0x01 } else { 0x00 };
    let ledc_b11 = if is_916en_or_917 { 0x02 } else { 0x00 };
    let ledc = [
        0xFC, 0x06, 0x00, 0x01, 0, 0, 0, 0, 0, 0, ledc_b10, ledc_b11, b'L', b'E', b'D', b'C',
    ];
    let exit_b1 = if is_new_gen { 0x04 } else { 0x03 };
    let exit_tepf = [
        0xFC, exit_b1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, b'T', b'E', b'P', b'F',
    ];
    (sectors, ledc, exit_tepf)
}

/// Parses an Innostor 512-byte `NAND` reply into `(primary_fid, record, active_ce_count, ch0_slots, ch1_slots)`.
/// Supports both 16-byte-per-CE layouts (Ch0 @ 0x00..0x80, Ch1 @ 0x80..0x100 or interleaved even/odd 8-byte slots).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_innostor_nand_channels(
    buf: &[u8],
    db: &ChipDatabase,
) -> Option<([u8; 6], Option<FlashRecord>, usize, Vec<Option<String>>, Vec<Option<String>>)> {
    let (fid, rec, ces) = find_wdb_flash_in_payload(buf, db, &[0, 8, 16, 32, 64])?;
    let mut ch0: Vec<Option<String>> = Vec::with_capacity(8);
    let mut ch1: Vec<Option<String>> = Vec::with_capacity(8);
    let id4 = &fid[..4];

    // Collect all matching 6-byte occurrences aligned to 8 or 16 bytes across the first 256 bytes
    let mut hit_offsets = Vec::new();
    let scan_end = buf.len().min(256).saturating_sub(5);
    let mut off = 0usize;
    while off < scan_end {
        if &buf[off..off + 4] == id4 {
            hit_offsets.push(off);
            off += 6;
        } else {
            off += 1;
        }
    }

    let fmt6 = |o: usize| -> String {
        format!(
            "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            buf[o], buf[o + 1], buf[o + 2], buf[o + 3], buf[o + 4], buf[o + 5]
        )
    };

    if hit_offsets.len() >= 2 {
        // Group into 2 channels (up to 8 slots each) when dual-channel count is even & >= 2
        let split_dual = hit_offsets.len() % 2 == 0 && hit_offsets.len() >= 2;
        if split_dual {
            let per_ch = (hit_offsets.len() / 2).min(8);
            for i in 0..8 {
                if i < per_ch {
                    ch0.push(Some(fmt6(hit_offsets[i])));
                    ch1.push(Some(fmt6(hit_offsets[per_ch + i])));
                } else {
                    ch0.push(None);
                    ch1.push(None);
                }
            }
            return Some((fid, Some(rec), per_ch, ch0, ch1));
        }
    }

    // Single-channel fallback
    for i in 0..8 {
        if i < hit_offsets.len() {
            ch0.push(Some(fmt6(hit_offsets[i])));
        } else {
            ch0.push(None);
        }
        ch1.push(None);
    }
    Some((fid, Some(rec), ces.max(hit_offsets.len()), ch0, ch1))
}

/// Runs the clean-room Innostor RAM-mode deep Flash ID probe on Windows using a user-supplied
/// official Innostor MP-tool RAM applet `.bin` file (e.g., `IS902E.bin` / `IN902E.bin`):
/// `FSCTL_DISMOUNT_VOLUME` -> `FSCTL_LOCK_VOLUME` -> `TEPF` -> `EXEC` (DATA_OUT) -> `NAND` (512B)
/// -> `LEDC` -> `TEPF (exit)` -> `START STOP UNIT (1B)` -> `FSCTL_UNLOCK_VOLUME`.
#[cfg(target_os = "windows")]
pub fn run_innostor_deep_probe(
    disk: &DiskInfo,
    db: &ChipDatabase,
    chip_model: &str,
    applet_bytes: &[u8],
    insp: &mut ChipInspection,
) {
    use crate::disk::direct_io::AlignedBuffer;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const FSCTL_LOCK_VOLUME: u32 = 0x00090018;
    const FSCTL_UNLOCK_VOLUME: u32 = 0x0009001C;
    const FSCTL_DISMOUNT_VOLUME: u32 = 0x00090020;
    const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x0004D014;
    const SCSI_IOCTL_DATA_OUT: u8 = 0;
    const SCSI_IOCTL_DATA_IN: u8 = 1;
    const SCSI_IOCTL_DATA_UNSPECIFIED: u8 = 2;

    #[repr(C)]
    struct Sptd {
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
    struct SptdWithSense {
        sptd: Sptd,
        sense_buf: [u8; 32],
    }

    if disk.is_system {
        insp.probe_trace
            .push("拒绝在系统盘上执行银灿 RAM 深度探测。".to_string());
        return;
    }

    let mut path_candidates = Vec::new();
    let mount_str = disk.mount_point.to_string_lossy();
    if mount_str.len() >= 2 && mount_str.as_bytes()[1] == b':' {
        path_candidates.push(format!(r"\\.\{}:", &mount_str[..1]));
    }
    if let Some(ref raw) = disk.raw_disk_path {
        path_candidates.push(raw.clone());
    }

    let mut file_opt = None;
    for p in &path_candidates {
        if let Ok(f) = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(p)
        {
            file_opt = Some(f);
            break;
        }
    }
    let Some(file) = file_opt else {
        insp.needs_windows_admin = true;
        insp.probe_trace
            .push("银灿 RAM 深度探测需要管理员权限打开设备句柄。".to_string());
        return;
    };
    let handle = file.as_raw_handle();

    let send_sptd = |cdb: &[u8], dir: u8, buf: Option<&mut [u8]>| -> bool {
        let sense_off = std::mem::offset_of!(SptdWithSense, sense_buf) as u32;
        let (ptr, len) = match buf {
            Some(b) => (b.as_mut_ptr() as *mut std::ffi::c_void, b.len() as u32),
            None => (std::ptr::null_mut(), 0),
        };
        let mut req = SptdWithSense {
            sptd: Sptd {
                length: std::mem::size_of::<Sptd>() as u16,
                scsi_status: 0,
                path_id: 0,
                target_id: 1,
                lun: 0,
                cdb_length: cdb.len().min(16) as u8,
                sense_info_length: 32,
                data_in: dir,
                data_transfer_length: len,
                time_out_value: 10,
                data_buffer: ptr,
                sense_info_offset: sense_off,
                cdb: [0; 16],
            },
            sense_buf: [0; 32],
        };
        for (i, &b) in cdb.iter().take(16).enumerate() {
            req.sptd.cdb[i] = b;
        }
        let mut ret: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_SCSI_PASS_THROUGH_DIRECT,
                &mut req as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<SptdWithSense>() as u32,
                &mut req as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<SptdWithSense>() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        ok != 0 && req.sptd.scsi_status == 0
    };

    let fsctl = |code: u32| -> bool {
        let mut ret: u32 = 0;
        unsafe {
            DeviceIoControl(
                handle,
                code,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                0,
                &mut ret,
                std::ptr::null_mut(),
            ) != 0
        }
    };

    let (sectors, ledc_cdb, exit_tepf_cdb) = innostor_ram_mode_params(chip_model);
    let (_, nand_cdb) = innostor_cdbs_for_model(chip_model);
    let total_bytes = (sectors as usize) * 512;

    if applet_bytes.is_empty() || applet_bytes.len() > total_bytes {
        insp.probe_trace.push(format!(
            "外部银灿固件体积不匹配 ({}B，预期 ≤ {}B)",
            applet_bytes.len(),
            total_bytes
        ));
        return;
    }

    let _ = fsctl(FSCTL_DISMOUNT_VOLUME);
    let _ = fsctl(FSCTL_LOCK_VOLUME);
    std::thread::sleep(std::time::Duration::from_millis(50));

    // 1. Enter temporary SRAM mode (TEPF)
    let enter_tepf = [0xFC, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b'T', b'E', b'P', b'F'];
    let tepf_ok = send_sptd(&enter_tepf, SCSI_IOCTL_DATA_UNSPECIFIED, None);
    insp.probe_trace.push(format!(
        "银灿 RAM 深度模式 TEPF 进入: {}",
        if tepf_ok { "成功" } else { "失败" }
    ));

    if tepf_ok {
        // 2. Upload RAM diagnostic applet padded to sectors*512 with 0xAA 0x55 trailer (EXEC)
        let alloc_len = ((total_bytes + 4095) / 4096) * 4096;
        let mut aligned_out = AlignedBuffer::new(alloc_len);
        let out_slice = &mut aligned_out.as_mut_slice()[..total_bytes];
        out_slice.fill(0xFF);
        out_slice[..applet_bytes.len()].copy_from_slice(applet_bytes);
        out_slice[total_bytes - 2] = 0xAA;
        out_slice[total_bytes - 1] = 0x55;

        let exec_cdb = [
            0xFC, 0x02, 0, 0, 0, 0, 0, 0, sectors, 0, 0, 0, b'E', b'X', b'E', b'C',
        ];
        let exec_ok = send_sptd(&exec_cdb, SCSI_IOCTL_DATA_OUT, Some(out_slice));
        insp.probe_trace.push(format!(
            "银灿 RAM 固件加载 EXEC ({}B): {}",
            total_bytes,
            if exec_ok { "成功" } else { "失败" }
        ));

        if exec_ok {
            std::thread::sleep(std::time::Duration::from_millis(60));
            // 3. Query 512-byte dual-channel NAND Flash ID table
            let mut aligned_in = AlignedBuffer::new(4096);
            let in_slice = &mut aligned_in.as_mut_slice()[..512];
            if send_sptd(&nand_cdb, SCSI_IOCTL_DATA_IN, Some(in_slice)) {
                if let Some((fid, rec_opt, ces, ch0, ch1)) =
                    parse_innostor_nand_channels(in_slice, db)
                {
                    let hex = format!(
                        "{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                        fid[0], fid[1], fid[2], fid[3], fid[4], fid[5]
                    );
                    insp.flash_id_hex = Some(hex.clone());
                    insp.flash_ce_count = Some(ces);
                    insp.flash_channel_map = Some((ch0, ch1));
                    insp.flash_record = rec_opt
                        .or_else(|| db.lookup_flash_id(&hex))
                        .or_else(|| decode_flash_id_algorithmic(&fid));
                    insp.probe_status =
                        "已通过银灿 RAM 深度诊断模式成功直读物理闪存 ID 与双通道映射表！".to_string();
                    insp.probe_trace.push(format!(
                        "银灿 RAM NAND 命中闪存: {} [{}CE]",
                        hex, ces
                    ));
                } else {
                    let head: String = in_slice
                        .iter()
                        .take(16)
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    insp.probe_trace
                        .push(format!("银灿 RAM NAND 返回 512B (前16B: {})", head));
                }
            }
        }

        // 4. ALWAYS cleanly exit RAM mode back to normal Mass Storage firmware!
        let _ = send_sptd(&ledc_cdb, SCSI_IOCTL_DATA_IN, None);
        let _ = send_sptd(&exit_tepf_cdb, SCSI_IOCTL_DATA_UNSPECIFIED, None);
        let start_unit = [0x1B, 0x00, 0x00, 0x00, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let _ = send_sptd(&start_unit, SCSI_IOCTL_DATA_UNSPECIFIED, None);
    }

    let _ = fsctl(FSCTL_UNLOCK_VOLUME);
    drop(file);
}

#[cfg(not(target_os = "windows"))]
pub fn run_innostor_deep_probe(
    _disk: &DiskInfo,
    _db: &ChipDatabase,
    _chip_model: &str,
    _applet_bytes: &[u8],
    insp: &mut ChipInspection,
) {
    insp.probe_status = "银灿 RAM 深度探测仅支持 Windows 平台。".to_string();
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
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

/// Scans a controller SCSI response payload for an exact 4-byte Flash ID match in `Chips.wdb`,
/// returning the 6-byte Flash ID, matched `FlashRecord`, and active CE count.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn find_wdb_flash_in_payload(
    buf: &[u8],
    db: &ChipDatabase,
    preferred_offsets: &[usize],
) -> Option<([u8; 6], FlashRecord, usize)> {
    // 1. Check preferred controller-specific offsets first (fastest & supports exact multi-CE counting)
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

    // 2. Scan whole payload at 1-byte granularity against all exact 4-byte IDs in Chips.wdb + supplement
    let limit = buf.len().saturating_sub(5);
    for i in 0..limit {
        if let Some(rec) = db.lookup_exact_4bytes(&buf[i..i + 4]) {
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
    // Include stride 32 (SMI F0 06: 0x30/0x50/0x70), stride 16 (SMI F0 20 / Alcor FA 00 / FirstChip F1 00),
    // stride 8 (ASolid CA / ITE FD 80), stride 10 (HY6919 DF D6), and stride 6 (packed).
    for stride in [16usize, 32usize, 8usize, 10usize, 6usize] {
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
fn try_capture_flash(
    label: &str,
    buf: &[u8],
    preferred_offsets: &[usize],
    vendor_name: &str,
    db: &ChipDatabase,
    found_flash: &mut Option<([u8; 6], Option<FlashRecord>, usize)>,
    insp: &mut ChipInspection,
) -> bool {
    if found_flash.is_some() {
        return true;
    }
    // Ignore all-zero or all-0xFF dummy buffers
    if buf.is_empty() || buf.iter().all(|&b| b == 0x00) || buf.iter().all(|&b| b == 0xFF) {
        return false;
    }
    // 1st priority: exact 4-byte match in Chips.wdb + 2020-2026 supplement
    if let Some((fid, rec, ces)) = find_wdb_flash_in_payload(buf, db, preferred_offsets) {
        insp.probe_trace.push(format!(
            "{} 命中闪存库: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} [{}CE] -> {} {}",
            label,
            fid[0],
            fid[1],
            fid[2],
            fid[3],
            fid[4],
            fid[5],
            ces,
            rec.manufacturer,
            rec.flash_type
        ));
        *found_flash = Some((fid, Some(rec), ces));
        if insp.firmware_vendor_hint.is_none() && !vendor_name.is_empty() {
            insp.firmware_vendor_hint = Some(vendor_name.to_string());
        }
        return true;
    }
    // 2nd priority: check exact controller-specific offsets for brand-new JEDEC NAND Flash ID
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
            let algo_rec = decode_flash_id_algorithmic(&fid);
            insp.probe_trace.push(format!(
                "{} 读到裸 Flash ID (偏移 0x{:X}): {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} [{}CE]",
                label, off, fid[0], fid[1], fid[2], fid[3], fid[4], fid[5], ces
            ));
            *found_flash = Some((fid, algo_rec, ces));
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
    const SCSI_IOCTL_DATA_UNSPECIFIED: u8 = 2;

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

    /// Exact Windows SDK `SCSI_PASS_THROUGH` (buffered IOCTL 0x4D004 — used by 59 of 87 ChipGenius calls!).
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

    // Helper: sends 0-byte DATA_UNSPECIFIED command (e.g. Phison `06 1D 00 00 41 57` wakeup!)
    let send_cdb_no_data = |cdb: &[u8]| -> bool {
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
                data_in: SCSI_IOCTL_DATA_UNSPECIFIED,
                data_transfer_length: 0,
                time_out_value: 3,
                data_buffer: std::ptr::null_mut(),
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
        ok != 0 && req.sptd.scsi_status == 0
    };

    // Helper: sends DATA_IN via buffered IOCTL_SCSI_PASS_THROUGH (0x4D004) or DIRECT (0x4D014), with automatic fallback!
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
            } else if ok != 0 {
                Err(0x10000 | (req.sptd.scsi_status as u32))
            } else {
                Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(-1) as u32)
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
            } else if ok != 0 {
                Err(0x10000 | (req.spt.scsi_status as u32))
            } else {
                Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(-1) as u32)
            }
        };

        if prefer_buffered {
            try_buffered().or_else(|_| try_direct())
        } else {
            try_direct().or_else(|_| try_buffered())
        }
    };

    // 0. Standard SCSI INQUIRY (0x12, 96B)
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

    if disk.is_system {
        insp.probe_status = "系统启动盘跳过 U 盘主控私有指令探测。".to_string();
        drop(file);
        return;
    }

    let mut found_flash: Option<([u8; 6], Option<FlashRecord>, usize)> = None;
    let mut innostor_needs_ram_applet = false;

    // =========================================================================
    // STAGE 1: Exact ChipGenius v4.21 Unconditional Safe Probes (Families 1..8)
    // =========================================================================

    // ---- 1. Yeestor / SiliconGo / DM (得一微 / 硅格 / 点序: 0x00483A1A) ----
    if let Ok(buf) = send_cdb(&[0xA1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 36, false) {
        if let Some(model) = parse_yeestor_a1(&buf) {
            insp.probe_trace.push(format!("Yeestor/SG A1 命中主控: {}", model));
            insp.firmware_vendor_hint = Some("Yeestor / SiliconGo (得一微/硅格)".to_string());
            insp.firmware_controller_hint = Some(model);
        }
    }
    if found_flash.is_none() {
        if let Ok(buf) = send_cdb(&[0xA5, 0x00, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0], 32, false) {
            let _ = try_capture_flash(
                "Yeestor/SG A5",
                &buf,
                &[0, 8, 16],
                "Yeestor / SiliconGo (得一微/硅格)",
                db,
                &mut found_flash,
                insp,
            );
        }
    }

    // ---- 2. Silicon Motion (慧荣 SMI: 0x004300F8) ----
    // Exact ChipGenius order & offsets:
    //   F0 04 -> model at 0x190..0x1AE
    //   F0 20 (... 01 07) -> multi-CE Flash ID at 0x00, 0x10, 0x20, 0x30, 0x40, 0x50
    //   F0 2A -> SM3257EN model at 0x190..0x1AE
    //   F0 06 -> SM326x/327x/328x Flash ID at 0x30 (CE0), 0x50 (CE1), 0x70 (CE2)!!
    if found_flash.is_none() {
        let smi_probes: &[(&str, [u8; 16], &[usize])] = &[
            (
                "SMI F0 04 (Model)",
                [0xF0, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0],
                &[0, 0x10, 0x30, 0x50],
            ),
            (
                "SMI F0 20 (FlashID)",
                [0xF0, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x07, 0, 0, 0],
                &[0, 0x10, 0x20, 0x30, 0x40, 0x50],
            ),
            (
                "SMI F0 2A (SM3257EN)",
                [0xF0, 0x2A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0],
                &[0, 0x10, 0x30, 0x50],
            ),
            (
                "SMI F0 06 (SM326/327/328)",
                [0xF0, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0],
                &[0x30, 0x50, 0x00, 0x10, 0x20],
            ),
        ];
        for &(label, cdb, offs) in smi_probes {
            if let Ok(buf) = send_cdb(&cdb, 512, false) {
                if let Some(model) = parse_smi_model_block(&buf) {
                    insp.probe_trace.push(format!("{} 命中主控: {}", label, model));
                    insp.firmware_vendor_hint = Some("Silicon Motion (慧荣)".to_string());
                    insp.firmware_controller_hint = Some(model);
                }
                if try_capture_flash(
                    label,
                    &buf,
                    offs,
                    "Silicon Motion (慧荣)",
                    db,
                    &mut found_flash,
                    insp,
                ) {
                    break;
                }
            }
        }
    }

    // ---- 3. Phison (群联: 0x004B8332) ----
    // Exact ChipGenius 3-step sequence:
    //   Step A: 06 1D 00 00 41 57 ("AW" wakeup, DATA_UNSPECIFIED=2, 0B)
    //   Step B: 06 05 (12B, DIRECT, 528B) -> chip ID at 0x17E..0x17F, FW at 0x94..0x99
    //   Step C: 06 56 (12B, BUFFERED, 512B) -> Flash ID at 0x00..0x05!
    if found_flash.is_none() || insp.firmware_controller_hint.is_none() {
        let _ = send_cdb_no_data(&[0x06, 0x1D, 0, 0, 0x41, 0x57, 0, 0, 0, 0, 0, 0]);
        if let Ok(info) = send_cdb(&[0x06, 0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 528, false) {
            if let Some((model, fw)) = parse_phison_0605_info(&info) {
                insp.probe_trace
                    .push(format!("Phison 06 05 (528B) 命中主控: {}", model));
                insp.firmware_vendor_hint = Some("Phison (群联)".to_string());
                insp.firmware_controller_hint = Some(model);
                if fw.is_some() {
                    insp.firmware_version_hint = fw;
                }
            }
            let _ = try_capture_flash(
                "Phison 06 05",
                &info,
                &[0, 0x10, 0x80],
                "Phison (群联)",
                db,
                &mut found_flash,
                insp,
            );
        }
        if found_flash.is_none() {
            if let Ok(buf) = send_cdb(&[0x06, 0x56, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 512, true) {
                let _ = try_capture_flash(
                    "Phison 06 56",
                    &buf,
                    &[0, 16, 32],
                    "Phison (群联)",
                    db,
                    &mut found_flash,
                    insp,
                );
            }
        }
    }

    // ---- 4. Solid State System (鑫创 SSS: 0x0047C30A) ----
    // Exact ChipGenius v4.21 sequence:
    //   1) FF 02 (12B, 16B) -> buf[0] decodes exact 19-entry SSS6673..SSS6698 / TC58NC66xx model
    //   2) FF <03|07> 68 (12B, 512B) -> 0x03 for SSS6673 (code=0x71), 0x07 for SSS6675..6698, plus FF 00 68 fallback
    if found_flash.is_none() {
        let mut sss_chip_code: Option<u8> = None;
        if let Ok(buf) = send_cdb(&[0xFF, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16, true) {
            if let Some(&code) = buf.first() {
                if let Some(model) = decode_sss_chip_code(code) {
                    sss_chip_code = Some(code);
                    insp.probe_trace.push(format!(
                        "SSS FF 02 命中主控码 0x{:02X} -> {}",
                        code, model
                    ));
                    insp.firmware_vendor_hint = Some("Solid State System (鑫创)".to_string());
                    insp.firmware_controller_hint = Some(model.to_string());
                }
            }
            let _ = try_capture_flash(
                "SSS FF 02",
                &buf,
                &[0, 4, 8],
                "Solid State System (鑫创)",
                db,
                &mut found_flash,
                insp,
            );
        }
        if found_flash.is_none() {
            // Exact ChipGenius v4.21 IIf at 0x0047DB56: 0x03 for SSS6673 (0x71), 0x07 for newer SSS chips
            let sss_subcmds: &[(&str, u8)] = if sss_chip_code == Some(0x71) {
                &[("SSS FF 03 68", 0x03), ("SSS FF 07 68", 0x07), ("SSS FF 00 68", 0x00)]
            } else {
                &[("SSS FF 07 68", 0x07), ("SSS FF 03 68", 0x03), ("SSS FF 00 68", 0x00)]
            };
            for &(label, sub) in sss_subcmds {
                if let Ok(buf) = send_cdb(&[0xFF, sub, 0x68, 0, 0, 0, 0, 0, 0, 0, 0, 0], 512, true)
                {
                    if try_capture_flash(
                        label,
                        &buf,
                        &[0, 8, 16, 32],
                        "Solid State System (鑫创)",
                        db,
                        &mut found_flash,
                        insp,
                    ) {
                        break;
                    }
                }
            }
        }
    }

    // ---- 5. ITE / Usbest (联阳: 0x00457EEE) ----
    // Exact ChipGenius CDBs: 12 00 00 00 B4 00 (6B, 180B), F2 (10B, 34B), FD 10 (10B, 5B), FD 80 ... 06 ... 01 (16B, 64B)!
    if found_flash.is_none() {
        if let Ok(ext_inq) = send_cdb(&[0x12, 0x00, 0x00, 0x00, 0xB4, 0x00], 180, true) {
            if ext_inq.len() >= 0x28 {
                let tag = String::from_utf8_lossy(&ext_inq[0x24..0x28]);
                if tag.starts_with("UT") || tag.starts_with("IT") || tag.starts_with("CR") {
                    if let Some(m) = extract_ascii_chip_hint(&ext_inq[0x24..], &["UT16", "IT11", "UT3", "CR3"]) {
                        insp.probe_trace.push(format!("ITE 扩展 Inquiry 命中主控: {}", m));
                        insp.firmware_vendor_hint = Some("ITE / Usbest (联阳)".to_string());
                        insp.firmware_controller_hint = Some(m);
                    }
                }
            }
        }
        let ite_probes: &[(&str, &[u8], usize)] = &[
            ("ITE F2 (10B)", &[0xF2, 0, 0, 0, 0, 0, 0, 0, 0, 0], 34),
            ("ITE FD 10 (10B)", &[0xFD, 0x10, 0, 0, 0, 0, 0, 0, 0, 0], 6),
            (
                "ITE FD 80 (16B)",
                &[0xFD, 0x80, 0, 0, 0, 0, 0, 0, 0x06, 0, 0, 0, 0, 0x01, 0, 0],
                64,
            ),
        ];
        for &(label, cdb, len) in ite_probes {
            if let Ok(buf) = send_cdb(cdb, len, true) {
                if try_capture_flash(
                    label,
                    &buf,
                    &[0, 8, 16],
                    "ITE / Usbest (联阳)",
                    db,
                    &mut found_flash,
                    insp,
                ) {
                    break;
                }
            }
        }
    }

    // ---- 6. iCreate (创惟系: 0x004B2CCF) ----
    // Exact ChipGenius CDBs: 12 00 00 00 4F 00 (6B, 79B), C2 01 90 00 00 00 00 00 80 00 69 BA (12B, 512B)!
    if found_flash.is_none() {
        if let Ok(buf) = send_cdb(
            &[0xC2, 0x01, 0x90, 0, 0, 0, 0, 0, 0x80, 0, 0x69, 0xBA],
            512,
            true,
        ) {
            let _ = try_capture_flash(
                "iCreate C2 01 90",
                &buf,
                &[0, 8, 16],
                "iCreate (创惟系)",
                db,
                &mut found_flash,
                insp,
            );
        }
    }

    // ---- 7. Innostor IS918M (银灿 IS918M: 0x004BA5A3) ----
    // Exact 16-byte CDB with trailing `05 80 F8 FF`!
    if found_flash.is_none() {
        let is918m_cdb = [
            0xE0, 0x10, 0x06, 0x90, 0x00, 0x0E, 0x00, 0x00,
            0x01, 0x00, 0x8A, 0xA3, 0x05, 0x80, 0xF8, 0xFF,
        ];
        if let Ok(buf) = send_cdb(&is918m_cdb, 512, true) {
            if let Some(m) =
                extract_ascii_chip_hint(&buf, &["IS918M", "C203A", "BW_825C", "BW-C301A"])
            {
                insp.probe_trace
                    .push(format!("Innostor IS918M E0 命中主控: {}", m));
                insp.firmware_vendor_hint = Some("Innostor (银灿)".to_string());
                insp.firmware_controller_hint = Some(m);
            }
            let _ = try_capture_flash(
                "IS918M E0 10 06 90",
                &buf,
                &[0, 8, 16, 32],
                "Innostor (银灿)",
                db,
                &mut found_flash,
                insp,
            );
        }
    }

    // ---- 8. Innostor IS918M_GA / IS918EN (18002) & ASolid (0x004C7601 & 0x00429483) ----
    // Exact 16-byte CDBs with trailing `"AS"` (0x41 0x53)!
    if found_flash.is_none() {
        let mut detected_asolid = false;
        let asolid_id_probes: &[(&str, [u8; 16])] = &[
            (
                "ASolid/IS918 CA 00 AS",
                [0xCA, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
            ),
            (
                "ASolid CA 00 06 AS",
                [0xCA, 0x00, 0, 0, 0, 0, 0x06, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
            ),
        ];
        for &(label, cdb) in asolid_id_probes {
            if let Ok(buf) = send_cdb(&cdb, 512, true) {
                if let Some((vendor, model, fw)) = parse_asolid_is918_ca00(&buf) {
                    detected_asolid = true;
                    insp.probe_trace.push(format!(
                        "{} 命中主控: {} ({})",
                        label,
                        model,
                        fw.as_deref().unwrap_or("-")
                    ));
                    insp.firmware_vendor_hint = Some(vendor);
                    insp.firmware_controller_hint = Some(model);
                    if fw.is_some() {
                        insp.firmware_version_hint = fw;
                    }
                    // Log bytes 16..48 when non-zero for diagnostics on newer IS918EN builds
                    if buf.len() >= 48 && buf[16..48].iter().any(|&b| b != 0 && b != 0xFF) {
                        let ext_hex: String = buf[16..48]
                            .iter()
                            .map(|b| format!("{:02X}", b))
                            .collect::<Vec<_>>()
                            .join(" ");
                        insp.probe_trace
                            .push(format!("{} 扩展字节[16..48]: {}", label, ext_hex));
                    }
                }
                if try_capture_flash(
                    label,
                    &buf,
                    &[0, 8, 16, 24, 32, 48, 64],
                    "ASolid / Innostor (点序/银灿)",
                    db,
                    &mut found_flash,
                    insp,
                ) {
                    break;
                }
                if detected_asolid {
                    break;
                }
            }
        }

        // Flash ID read-only commands: always try the 2 canonical ChipGenius commands (CA 0F 00 01 & CA 04 01 01),
        // and when CA 00 confirmed an ASolid/IS918 controller (including 18002 / IS918EN), try full read-only CA variants!
        let mut asolid_flash_probes: Vec<(&str, [u8; 16])> = vec![
            (
                "IS918M_GA CA 0F 00 01 AS",
                [0xCA, 0x0F, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
            ),
            (
                "ASolid CA 04 01 01 06 AS",
                [0xCA, 0x04, 0x01, 0x01, 0, 0, 0x06, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
            ),
        ];
        if detected_asolid {
            asolid_flash_probes.extend_from_slice(&[
                (
                    "ASolid CA 00 01 00 06 AS",
                    [0xCA, 0x00, 0x01, 0x00, 0, 0, 0x06, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "IS918EN CA 0F 00 00 AS",
                    [0xCA, 0x0F, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "IS918EN CA 0F 01 00 AS",
                    [0xCA, 0x0F, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "IS918EN CA 0F 00 01 06 AS",
                    [0xCA, 0x0F, 0x00, 0x01, 0, 0, 0x06, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "ASolid CA 04 00 01 06 AS",
                    [0xCA, 0x04, 0x00, 0x01, 0, 0, 0x06, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "ASolid CA 04 00 00 AS",
                    [0xCA, 0x04, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "IS918EN CA 01 00 00 AS",
                    [0xCA, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "IS918EN CA 02 00 00 AS",
                    [0xCA, 0x02, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "IS918EN CA 06 00 00 AS",
                    [0xCA, 0x06, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "IS918EN CA 07 00 00 AS",
                    [0xCA, 0x07, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
                (
                    "IS918EN CA 0E 00 00 AS",
                    [0xCA, 0x0E, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41, 0x53],
                ),
            ]);
        }
        for (label, cdb) in asolid_flash_probes {
            if found_flash.is_some() {
                break;
            }
            match send_cdb(&cdb, 512, true) {
                Ok(buf) => {
                    if try_capture_flash(
                        label,
                        &buf,
                        &[0, 8, 16, 24, 32, 48, 64],
                        "ASolid / Innostor (点序/银灿)",
                        db,
                        &mut found_flash,
                        insp,
                    ) {
                        break;
                    }
                }
                Err(code) if detected_asolid => {
                    if (code & 0x10000) != 0 {
                        insp.probe_trace.push(format!(
                            "{} 拒绝 (SCSI_STATUS=0x{:02X})",
                            label,
                            code & 0xFF
                        ));
                    } else {
                        insp.probe_trace
                            .push(format!("{} 失败 (win_err={})", label, code));
                    }
                }
                Err(_) => {}
            }
        }
    }

    // =========================================================================
    // STAGE 2: Additional ChipGenius v4.21 Controller Families (Families 9..17)
    // =========================================================================

    // ---- 9. Alcor Micro (安国: 0x004503D5) ----
    // Exact ChipGenius sequence: 9A (1-byte CDB, 512B -> 48-entry chip code at buf[4..6]), FA 0E (8B, 512B), FA 00 (8B, 512B -> Flash ID stride 16)!
    if found_flash.is_none() {
        if let Ok(buf) = send_cdb(&[0x9A], 512, false) {
            if buf.len() >= 6 {
                let code = u16::from_be_bytes([buf[4], buf[5]]);
                if let Some(model) = decode_alcor_chip_code(code) {
                    insp.probe_trace.push(format!(
                        "Alcor 9A (1B CDB) 命中主控码 0x{:04X} -> {}",
                        code, model
                    ));
                    insp.firmware_vendor_hint = Some("Alcor Micro (安国)".to_string());
                    insp.firmware_controller_hint = Some(model.to_string());
                }
            }
            let _ = try_capture_flash(
                "Alcor 9A",
                &buf,
                &[0, 0x10, 0x20],
                "Alcor Micro (安国)",
                db,
                &mut found_flash,
                insp,
            );
        }
        let alcor_probes: &[(&str, &[u8])] = &[
            ("Alcor FA 0E", &[0xFA, 0x0E, 0, 0, 0, 0, 0, 0]),
            ("Alcor FA 00", &[0xFA, 0x00, 0, 0, 0, 0, 0, 0]),
        ];
        for &(label, cdb) in alcor_probes {
            if found_flash.is_some() {
                break;
            }
            if let Ok(buf) = send_cdb(cdb, 512, false) {
                if let Some(m) = extract_ascii_chip_hint(&buf, &["AU69", "AU64", "SC90", "SC70"]) {
                    insp.firmware_vendor_hint = Some("Alcor Micro (安国)".to_string());
                    insp.firmware_controller_hint = Some(m);
                }
                if try_capture_flash(
                    label,
                    &buf,
                    &[0, 0x10, 0x20, 0x30],
                    "Alcor Micro (安国)",
                    db,
                    &mut found_flash,
                    insp,
                ) {
                    break;
                }
            }
        }
    }

    // ---- 10. Chipsbank (芯邦: 0x0044553E) ----
    // Exact ChipGenius CDBs: E3 01 (12B, 128B), EA 01 ... E3 (16B, 1536B), EA 1B 00 01 ... E3 (16B, 4B), E6 00 ... E6 (12B/16B, 128B)!
    if found_flash.is_none() {
        let cbm_probes: &[(&str, &[u8], usize, &[usize])] = &[
            (
                "Chipsbank E3 01",
                &[0xE3, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                128,
                &[0, 8, 16, 32],
            ),
            (
                "Chipsbank EA 01",
                &[0xEA, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xE3],
                1536,
                &[0, 16, 32, 64, 512, 0x684],
            ),
            (
                "Chipsbank EA 1B",
                &[0xEA, 0x1B, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xE3],
                4,
                &[0],
            ),
            (
                "Chipsbank E6 00",
                &[0xE6, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xE6],
                128,
                &[0, 8, 16, 32],
            ),
        ];
        for &(label, cdb, len, offs) in cbm_probes {
            if let Ok(buf) = send_cdb(cdb, len, true) {
                if let Some(m) = parse_chipsbank_info(&buf) {
                    insp.probe_trace
                        .push(format!("{} 命中主控: {}", label, m));
                    insp.firmware_vendor_hint = Some("Chipsbank (芯邦)".to_string());
                    insp.firmware_controller_hint = Some(m);
                }
                if try_capture_flash(
                    label,
                    &buf,
                    offs,
                    "Chipsbank (芯邦)",
                    db,
                    &mut found_flash,
                    insp,
                ) {
                    break;
                }
            }
        }
    }

    // ---- 11. FirstChip (一芯: 0x004BD8AE) ----
    // Exact ChipGenius CDBs: F1 01 ... A0 (magic 1A 2B 3C 4D @ 0x20), F1 00 ... A1 (Flash ID @ 0x10, 0x20, 0x30)!
    if found_flash.is_none() {
        if let Ok(buf) = send_cdb(
            &[0xF1, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xA0],
            512,
            false,
        ) {
            if let Some(model) = parse_firstchip_f101(&buf) {
                insp.probe_trace
                    .push(format!("FirstChip F1 01 A0 命中主控: {}", model));
                insp.firmware_vendor_hint = Some("FirstChip (一芯)".to_string());
                insp.firmware_controller_hint = Some(model);
            }
            let _ = try_capture_flash(
                "FirstChip F1 01 A0",
                &buf,
                &[0x10, 0x20, 0x30, 0x00],
                "FirstChip (一芯)",
                db,
                &mut found_flash,
                insp,
            );
        }
        if found_flash.is_none() {
            if let Ok(buf) = send_cdb(
                &[0xF1, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xA1],
                512,
                false,
            ) {
                let _ = try_capture_flash(
                    "FirstChip F1 00 A1",
                    &buf,
                    &[0x10, 0x20, 0x30, 0x00],
                    "FirstChip (一芯)",
                    db,
                    &mut found_flash,
                    insp,
                );
            }
        }
    }

    // ---- 12. Innostor Classic IS902..IS917 (银灿: 0x0045D4F8) ----
    // Exact ChipGenius v4.21 sequence:
    //   1) FC 00 ... 80 00 00 "INQU" (128B) -> exact chip model (IS902/IS902E/IS903/IS916/IS916EN/IS917)
    //   2) FC <04|05> FE ... 06 00 00 "NFTL" (16B) -> 4-byte F/W version (e.g. 2.07.18.00)
    //   3) FC <04|03> 01 <02|00> ... 02 00 <08|04> 05 "NAND" (512B) -> multi-CE / dual-channel Flash ID table!
    if found_flash.is_none() {
        let inno_inqu = [0xFC, 0x00, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, b'I', b'N', b'Q', b'U'];
        let mut detected_inno_model: Option<String> = None;
        if let Ok(buf) = send_cdb(&inno_inqu, 128, true) {
            if let Some(m) = parse_innostor_inqu(&buf) {
                insp.probe_trace
                    .push(format!("Innostor FC INQU 命中主控: {}", m));
                insp.firmware_vendor_hint = Some("Innostor (银灿)".to_string());
                insp.firmware_controller_hint = Some(m.clone());
                detected_inno_model = Some(m);
            }
        }

        // Build exact prioritized (NFTL, NAND) CDB order based on detected chip (or try all 3 generations)
        let mut nftl_list: Vec<(&str, [u8; 16])> = Vec::new();
        let mut nand_list: Vec<(&str, [u8; 16])> = Vec::new();
        if let Some(ref model) = detected_inno_model {
            let (nftl_exact, nand_exact) = innostor_cdbs_for_model(model);
            nftl_list.push(("Innostor FC NFTL (精确型号)", nftl_exact));
            nand_list.push(("Innostor FC NAND (精确型号)", nand_exact));
        }
        // Fallback variants in case INQU didn't return ASCII or chip is a hybrid revision
        let (_, nand_902) = innostor_cdbs_for_model("IS902E");
        let (_, nand_903) = innostor_cdbs_for_model("IS903");
        let (_, nand_917) = innostor_cdbs_for_model("IS917");
        for (lbl, cdb) in [
            ("Innostor FC NAND (IS902/902E/916)", nand_902),
            ("Innostor FC NAND (IS903)", nand_903),
            ("Innostor FC NAND (IS916EN/917)", nand_917),
        ] {
            if !nand_list.iter().any(|(_, c)| c == &cdb) {
                nand_list.push((lbl, cdb));
            }
        }

        for &(label, cdb) in &nftl_list {
            match send_cdb(&cdb, 16, false) {
                Ok(buf) => {
                    if let Some(fw) = parse_innostor_nftl_fw(&buf) {
                        insp.probe_trace
                            .push(format!("{} 读到固件版本: F/W {}", label, fw));
                        insp.firmware_version_hint = Some(fw);
                    }
                    let _ = try_capture_flash(
                        label,
                        &buf,
                        &[0, 6, 8],
                        "Innostor (银灿)",
                        db,
                        &mut found_flash,
                        insp,
                    );
                }
                Err(code) if detected_inno_model.is_some() => {
                    insp.probe_trace
                        .push(format!("{} 失败 (code={})", label, code));
                }
                Err(_) => {}
            }
        }

        // If Innostor runtime firmware advertises "PINF" (Product Info), probe it read-only too
        if detected_inno_model.is_some() && found_flash.is_none() {
            for (lbl, cdb) in [
                (
                    "Innostor FC PINF (0x00)",
                    [0xFC, 0x00, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, b'P', b'I', b'N', b'F'],
                ),
                (
                    "Innostor FC PINF (0x04)",
                    [0xFC, 0x04, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, b'P', b'I', b'N', b'F'],
                ),
            ] {
                if let Ok(buf) = send_cdb(&cdb, 128, true) {
                    if try_capture_flash(
                        lbl,
                        &buf,
                        &[0, 8, 16, 32],
                        "Innostor (银灿)",
                        db,
                        &mut found_flash,
                        insp,
                    ) {
                        break;
                    }
                }
            }
        }

        for &(label, cdb) in &nand_list {
            if found_flash.is_some() {
                break;
            }
            match send_cdb(&cdb, 512, false) {
                Ok(buf) => {
                    if try_capture_flash(
                        label,
                        &buf,
                        &[0, 8, 16, 32, 64],
                        "Innostor (银灿)",
                        db,
                        &mut found_flash,
                        insp,
                    ) {
                        break;
                    }
                }
                Err(code) if detected_inno_model.is_some() => {
                    if (code & 0x10000) != 0 {
                        insp.probe_trace.push(format!(
                            "{} 被固件拒绝 (SCSI_STATUS=0x{:02X}，运行态未开放 NAND 指令)",
                            label,
                            code & 0xFF
                        ));
                    } else {
                        insp.probe_trace
                            .push(format!("{} 失败 (win_err={})", label, code));
                    }
                }
                Err(_) => {}
            }
        }

        if detected_inno_model.is_some() && found_flash.is_none() {
            innostor_needs_ram_applet = true;
            insp.innostor_deep_probe_model = detected_inno_model.clone();
        }
    }

    // ---- 13. Micov / Ameco (迈科微: 0x0044CDE9) ----
    // Exact ChipGenius CDBs: D3 00 (16B, 512B), D6 00 90 08 00 00 01 (16B, 16B — handles 16-bit interleaved Flash ID!)
    if found_flash.is_none() {
        if let Ok(buf) = send_cdb(&[0xD3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 512, true) {
            let _ = try_capture_flash(
                "Micov D3 00",
                &buf,
                &[0, 16, 32],
                "Micov (迈科微)",
                db,
                &mut found_flash,
                insp,
            );
        }
        if found_flash.is_none() {
            if let Ok(buf) = send_cdb(
                &[0xD6, 0x00, 0x90, 0x08, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                16,
                true,
            ) {
                if !try_capture_flash(
                    "Micov D6 00 90",
                    &buf,
                    &[0, 4, 8],
                    "Micov (迈科微)",
                    db,
                    &mut found_flash,
                    insp,
                ) {
                    if let Some((even, odd)) = deinterleave_micov_d6(&buf) {
                        if !try_capture_flash(
                            "Micov D6 (16-bit Even)",
                            &even,
                            &[0],
                            "Micov (迈科微)",
                            db,
                            &mut found_flash,
                            insp,
                        ) {
                            let _ = try_capture_flash(
                                "Micov D6 (16-bit Odd)",
                                &odd,
                                &[0],
                                "Micov (迈科微)",
                                db,
                                &mut found_flash,
                                insp,
                            );
                        }
                    }
                }
            }
        }
    }

    // ---- 14. HY6919 (华义: 0x004C2321) ----
    // Exact ChipGenius read-only CDBs: DF ... D3 (16B, 512B -> controller + FW at 0x21..0x24), DF 00 08 ... D6 (16B, 512B, stride 10)!
    if found_flash.is_none() {
        let hy_d3 = [0xDF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xD3];
        if let Ok(buf) = send_cdb(&hy_d3, 512, true) {
            if let Some((model, fw)) = parse_hy6919_d3(&buf) {
                insp.probe_trace
                    .push(format!("HY6919 DF D3 命中主控: {}", model));
                insp.firmware_vendor_hint = Some("HY (华义)".to_string());
                insp.firmware_controller_hint = Some(model);
                if fw.is_some() {
                    insp.firmware_version_hint = fw;
                }
            }
            let _ = try_capture_flash(
                "HY6919 DF D3",
                &buf,
                &[0, 8, 16],
                "HY6919",
                db,
                &mut found_flash,
                insp,
            );
        }
        if found_flash.is_none() {
            let hy_d6 = [0xDF, 0, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xD6];
            if let Ok(buf) = send_cdb(&hy_d6, 512, true) {
                let _ = try_capture_flash(
                    "HY6919 DF D6",
                    &buf,
                    &[0, 10, 20],
                    "HY6919",
                    db,
                    &mut found_flash,
                    insp,
                );
            }
        }
    }

    // ---- 15. Appotech / Netac / SanDisk (0x004B5FCD) ----
    // Exact ChipGenius CDBs: F5 01 (24B), F5 09 (24B), F5 00 ... 08 ... (256B)!
    if found_flash.is_none() {
        let appo_probes: &[(&str, &[u8], usize)] = &[
            ("Appotech F5 01", &[0xF5, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 24),
            ("Appotech F5 09", &[0xF5, 0x09, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 24),
            (
                "Appotech F5 00 08",
                &[0xF5, 0x00, 0, 0, 0, 0, 0, 0, 0x08, 0, 0, 0],
                256,
            ),
        ];
        for &(label, cdb, len) in appo_probes {
            if let Ok(buf) = send_cdb(cdb, len, true) {
                if let Some(m) =
                    extract_ascii_chip_hint(&buf, &["APPOTECH", "PMAP", "SMHD", "BWSE"])
                {
                    insp.firmware_controller_hint = Some(m);
                }
                if try_capture_flash(
                    label,
                    &buf,
                    &[0, 8, 16],
                    "Appotech / Netac",
                    db,
                    &mut found_flash,
                    insp,
                ) {
                    break;
                }
            }
        }
    }

    // ---- 16. Skymedi (擎泰: 0x0046E14A — safe read-only CA/C8 10-byte CDBs) ----
    if found_flash.is_none() {
        let sk_probes: &[(&str, &[u8], bool)] = &[
            ("Skymedi CA (10B)", &[0xCA, 0, 0, 0, 0, 0, 0, 0, 0, 0], false),
            ("Skymedi C8 (10B)", &[0xC8, 0, 0, 0, 0, 0, 0, 0, 0, 0], true),
        ];
        for &(label, cdb, buffered) in sk_probes {
            if let Ok(buf) = send_cdb(cdb, 512, buffered) {
                if let Some(m) = parse_skymedi_ca(&buf) {
                    insp.probe_trace
                        .push(format!("{} 命中主控: {}", label, m));
                    insp.firmware_vendor_hint = Some("Skymedi (擎泰)".to_string());
                    insp.firmware_controller_hint = Some(m);
                }
                if try_capture_flash(
                    label,
                    &buf,
                    &[0, 8, 16, 32],
                    "Skymedi (擎泰)",
                    db,
                    &mut found_flash,
                    insp,
                ) {
                    break;
                }
            }
        }
    }

    // ---- 17. SAT-16 ATA IDENTIFY DEVICE (USB-SATA / USB-NVMe Bridges & SSD Flash Drives) ----
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

    // ---- Final Result Assembly ----
    if let Some((fid, rec_opt, ces)) = found_flash {
        let hex = format!(
            "{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
            fid[0], fid[1], fid[2], fid[3], fid[4], fid[5]
        );
        insp.flash_record = rec_opt
            .or_else(|| db.lookup_flash_id(&hex))
            .or_else(|| decode_flash_id_algorithmic(&fid));
        if insp.flash_record.is_none() {
            insp.flash_vendor_fallback = Some(nand_manufacturer_name(fid[0]).to_string());
        }
        insp.flash_id_hex = Some(hex);
        insp.flash_ce_count = Some(ces);
        insp.probe_status =
            "已通过 ChipGenius v4.21 级底层 SCSI 穿透引擎成功直读主控与物理闪存 Flash ID！".to_string();
    } else if innostor_needs_ram_applet {
        insp.probe_status =
            "已直读银灿主控型号与固件版本！该主控运行态固件默认未开放只读 NAND 指令（ChipGenius 需先强制卸载盘符并向主控 SRAM 临时注入银灿专有 8KB 诊断小程序后才能读出闪存 ID；本软件出于数据安全默认仅执行 100% 无损只读查询）。"
                .to_string();
    } else if insp.firmware_controller_hint.is_some() {
        insp.probe_status =
            "已通过底层 SCSI 穿透直读到主控型号，但该固件未返回可识别的裸闪存 Flash ID（可展开下方「底层探测诊断明细」查看原始返回字节）。"
                .to_string();
    } else if insp.bridge_ata_model.is_some() {
        insp.probe_status =
            "已通过 SAT-16 穿透读取到 USB 桥接内置固态硬盘型号与固件版本！".to_string();
    } else {
        insp.probe_status =
            "已完成全部 17 大主控家族 SCSI 穿透探测；当前设备未响应公开私有指令（常见于原厂加密主控，可展开下方「底层探测诊断明细」反馈原始返回信息）。"
                .to_string();
    }

    drop(file);

    // If an Innostor IS902~IS917 drive needs a RAM applet AND the user placed `innostor/<CHIP>.bin`
    // beside the executable, run it automatically!
    if insp.flash_id_hex.is_none() {
        if let Some(model) = insp.innostor_deep_probe_model.clone() {
            let base = model.split('-').next().unwrap_or(&model);
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let candidates = [
                        dir.join("innostor").join(format!("{}.bin", base)),
                        dir.join("innostor").join(format!("IN{}.bin", base.trim_start_matches("IS"))),
                        dir.join(format!("{}.bin", base)),
                    ];
                    for p in candidates {
                        if let Ok(bytes) = std::fs::read(&p) {
                            insp.probe_trace
                                .push(format!("检测到外部银灿固件: {}", p.display()));
                            run_innostor_deep_probe(disk, db, &model, &bytes, insp);
                            break;
                        }
                    }
                }
            }
        }
    }
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
    fn phison_0605_parser_reads_0x17e_and_fw_date() {
        let mut info = vec![0u8; 528];
        info[0x17E] = 0x23;
        info[0x17F] = 0x19;
        info[0x94] = 0x09;
        info[0x95] = 0x02;
        info[0x96] = 0x53;
        info[0x97] = 21;
        info[0x98] = 6;
        info[0x99] = 15;
        let (model, fw) = parse_phison_0605_info(&info).expect("phison 0605");
        assert_eq!(model, "PS2251-19 (PS2319)");
        assert_eq!(fw.as_deref(), Some("09.02.53 [2021-06-15]"));
    }

    #[test]
    fn alcor_48_entry_chip_code_table_works() {
        assert_eq!(decode_alcor_chip_code(0xEC05), Some("AU6989SN-GT"));
        assert_eq!(decode_alcor_chip_code(0xEF01), Some("AU6989SN-GTA"));
        assert_eq!(decode_alcor_chip_code(0xF8C0), Some("AU6989SN-GTE"));
        assert_eq!(decode_alcor_chip_code(0xFA80), Some("AU6989SN-GTF"));
        assert_eq!(decode_alcor_chip_code(0xE642), Some("SC908 <Special>"));
        assert_eq!(decode_alcor_chip_code(0x0000), None);
    }

    #[test]
    fn smi_f004_model_and_f006_stride32_flash_work() {
        let db = ChipDatabase::load_default();
        let mut f004 = vec![0u8; 512];
        f004[0x190..0x198].copy_from_slice(b"SM3281BB");
        assert_eq!(parse_smi_model_block(&f004).as_deref(), Some("SM3281BB"));

        // SMI F0 06 puts CE0 at 0x30 and CE1 at 0x50 (stride 0x20 = 32)
        let mut f006 = vec![0u8; 512];
        f006[0x30..0x36].copy_from_slice(&[0x98, 0x3A, 0x98, 0xB3, 0x76, 0x72]);
        f006[0x50..0x56].copy_from_slice(&[0x98, 0x3A, 0x98, 0xB3, 0x76, 0x72]);
        let (fid, rec, ces) =
            find_wdb_flash_in_payload(&f006, &db, &[0x30, 0x50]).expect("SMI F0 06 flash");
        assert_eq!(fid, [0x98, 0x3A, 0x98, 0xB3, 0x76, 0x72]);
        assert_eq!(rec.flash_type, "TLC-16K");
        assert_eq!(ces, 2);
    }

    #[test]
    fn firstchip_and_micov_deinterleave_work() {
        let mut fc = vec![0u8; 512];
        fc[0x20..0x24].copy_from_slice(&[0x1A, 0x2B, 0x3C, 0x4D]);
        fc[0x30..0x38].copy_from_slice(b"chip2017");
        assert_eq!(parse_firstchip_f101(&fc).as_deref(), Some("FC1178BC"));

        // Micov D6 16-bit interleaved bus: even bytes = 98 3A 98 B3 76 72
        let d6 = [0x98, 0x00, 0x3A, 0x00, 0x98, 0x00, 0xB3, 0x00, 0x76, 0x00, 0x72, 0x00];
        let (even, _odd) = deinterleave_micov_d6(&d6).expect("micov d6");
        assert_eq!(even, [0x98, 0x3A, 0x98, 0xB3, 0x76, 0x72]);
    }

    #[test]
    fn innostor_cdbs_and_intel_slc_flash_match_exact_chipgenius() {
        let db = ChipDatabase::load_default();
        // IS902E exact CDBs from ChipGenius v4.21 (0x0045EF5D / 0x0045FAD8 / 0x0045FCFE)
        let (nftl_902e, nand_902e) = innostor_cdbs_for_model("IS902E-A1");
        assert_eq!(nftl_902e[1], 0x04);
        assert_eq!(
            nand_902e,
            [0xFC, 0x04, 0x01, 0x02, 0, 0, 0, 0, 0x02, 0, 0x08, 0x05, b'N', b'A', b'N', b'D']
        );

        let (nftl_903, nand_903) = innostor_cdbs_for_model("IS903");
        assert_eq!(nftl_903[1], 0x05);
        assert_eq!(nand_903[1], 0x03);
        assert_eq!(nand_903[3], 0x02);
        assert_eq!(nand_903[10], 0x04);

        let (_, nand_917) = innostor_cdbs_for_model("IS917");
        assert_eq!(nand_917[1], 0x03);
        assert_eq!(nand_917[3], 0x00);
        assert_eq!(nand_917[10], 0x04);

        // BCD hex: 0x02, 0x07, 0x18, 0x00 -> "2.07.18.00" (not decimal 2.07.24.00!)
        assert_eq!(
            parse_innostor_nftl_fw(&[0x02, 0x07, 0x18, 0x00, 0x81, 0x06]).as_deref(),
            Some("2.07.18.00")
        );
        // INQU byte 0 = 0xA1 -> "IS902E-A1"
        let mut inqu_reply = vec![0u8; 128];
        inqu_reply[0] = 0xA1;
        inqu_reply[16..22].copy_from_slice(b"IS902E");
        assert_eq!(parse_innostor_inqu(&inqu_reply).as_deref(), Some("IS902E-A1"));

        // Simulate IS902E 512B NAND response with 4CE Intel JS29F16B08HCND1 (89 68 21 A6 A9 00)
        let mut nand_reply = vec![0u8; 512];
        for ce in 0..4 {
            let off = 16 + ce * 16;
            nand_reply[off..off + 6].copy_from_slice(&[0x89, 0x68, 0x21, 0xA6, 0xA9, 0x00]);
        }
        let (fid, rec, ces) =
            find_wdb_flash_in_payload(&nand_reply, &db, &[0, 8, 16, 32, 64]).expect("Intel SLC");
        assert_eq!(fid, [0x89, 0x68, 0x21, 0xA6, 0xA9, 0x00]);
        assert!(rec.manufacturer.contains("Intel"));
        assert_eq!(rec.flash_type, "SLC-4K");
        assert!(rec.part_numbers.iter().any(|p| p.contains("JS29F16B08HCND1")));
        assert_eq!(ces, 4);

        // Dual-channel 8-slot (2x4CE) Innostor NAND channel map test
        let mut dual_reply = vec![0u8; 512];
        for i in 0..8 {
            let off = i * 16;
            dual_reply[off..off + 6].copy_from_slice(&[0x89, 0x68, 0x21, 0xA6, 0xA9, 0x00]);
        }
        let (_, _, per_ch_ce, ch0, ch1) =
            parse_innostor_nand_channels(&dual_reply, &db).expect("dual channel map");
        assert_eq!(per_ch_ce, 4);
        assert_eq!(ch0[0].as_deref(), Some("896821A6A900"));
        assert_eq!(ch1[3].as_deref(), Some("896821A6A900"));
        assert_eq!(ch0[4], None);
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

    #[test]
    fn sss_skymedi_chipsbank_hy6919_decoders_work() {
        // SSS 19-entry FF 02 chip code table
        assert_eq!(decode_sss_chip_code(0x71), Some("SSS6673"));
        assert_eq!(decode_sss_chip_code(0xB0), Some("TC58NC6688/SSS6690"));
        assert_eq!(decode_sss_chip_code(0xBA), Some("TC58NC6623/SSS6698"));
        assert_eq!(decode_sss_chip_code(0xBB), Some("SSS6698"));
        assert_eq!(decode_sss_chip_code(0x00), None);

        // Skymedi CA big-endian 32-bit signature + SK6226AA/BA byte 8
        let mut sk = vec![0u8; 32];
        sk[..4].copy_from_slice(&0x16030801u32.to_be_bytes());
        assert_eq!(parse_skymedi_ca(&sk).as_deref(), Some("SK6281AA"));
        sk[..4].copy_from_slice(&0x12261102u32.to_be_bytes());
        sk[8] = 0;
        assert_eq!(parse_skymedi_ca(&sk).as_deref(), Some("SK6226AA"));
        sk[8] = 1;
        assert_eq!(parse_skymedi_ca(&sk).as_deref(), Some("SK6226BA"));
        sk[..4].copy_from_slice(&0x12310832u32.to_be_bytes());
        assert_eq!(parse_skymedi_ca(&sk).as_deref(), Some("SK6231"));

        // Chipsbank EA 01 tags & build-date disambiguation
        assert_eq!(
            parse_chipsbank_info(b"CHIP2098A_FW").as_deref(),
            Some("CBM2098A(V98A)")
        );
        assert_eq!(
            parse_chipsbank_info(b"CHIP 209XE 2017  10  01").as_deref(),
            Some("CBM2199S")
        );

        // HY6919 DF D3 controller & FW decoder
        let mut hy = vec![0u8; 64];
        hy[0..6].copy_from_slice(b"HY6919");
        hy[0x21..0x25].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        let (m, fw) = parse_hy6919_d3(&hy).expect("HY6919 D3");
        assert_eq!(m, "HY6919");
        assert_eq!(fw.as_deref(), Some("01.02.03.04"));

        // Real Kingston DataTraveler 3.0 ASolid / IS918EN (18002SO3T_4A1002) CA 00 response
        let mut kt = vec![0u8; 512];
        kt[..16].copy_from_slice(b"18002SO3T_4A1002");
        let (v, cm, cfw) = parse_asolid_is918_ca00(&kt).expect("IS918EN 18002");
        assert!(v.contains("ASolid"));
        assert_eq!(cm, "IS918EN (ASolid 18002)");
        assert_eq!(cfw.as_deref(), Some("18002SO3T_4A1002"));

        let mut ga = vec![0u8; 64];
        ga[..16].copy_from_slice(b"13008SE3L_000001");
        let (_, ga_m, ga_fw) = parse_asolid_is918_ca00(&ga).expect("IS918M_GA");
        assert_eq!(ga_m, "IS918M_GA");
        assert_eq!(ga_fw.as_deref(), Some("13008SE3L_000001"));
    }
}
