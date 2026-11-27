use crate::disk::direct_io::get_volume_space;
use crate::disk::{DiskInfo, DriveKind};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Default)]
struct UsbHardwareInfo {
    vid: Option<String>,
    pid: Option<String>,
    bcd_device: Option<String>,
    bcd_usb: Option<String>,
    bcd_usb_raw: Option<u16>,
    max_power_ma: Option<u32>,
    manufacturer: Option<String>,
    serial: Option<String>,
    speed: Option<String>,
    product_name: Option<String>,
    scsi_vendor: Option<String>,
    scsi_product: Option<String>,
    scsi_rev: Option<String>,
}

pub fn scan_disks_macos() -> Vec<DiskInfo> {
    let usb_map = query_usb_hardware_map();
    let mut results = Vec::new();
    let mut seen_mounts = std::collections::HashSet::new();

    // 1. Scan /Volumes/* (external USB, SD cards, external SSDs, mounted DMGs)
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Skip symlink pointing to root "/" (e.g. Macintosh HD symlink in /Volumes)
            if let Ok(canon) = std::fs::canonicalize(&path) {
                if canon == Path::new("/") {
                    continue;
                }
            }
            if let Some(info) = inspect_mount_macos(&path, false, &usb_map) {
                seen_mounts.insert(info.mount_point.clone());
                results.push(info);
            }
        }
    }

    // 2. Also include root "/" (marked as system disk so it's visible but safety-locked)
    let root = PathBuf::from("/");
    if !seen_mounts.contains(&root) {
        if let Some(info) = inspect_mount_macos(&root, true, &usb_map) {
            results.push(info);
        }
    }

    results
}

fn inspect_mount_macos(
    mount_point: &Path,
    is_root: bool,
    usb_map: &HashMap<String, UsbHardwareInfo>,
) -> Option<DiskInfo> {
    let output = Command::new("diskutil")
        .arg("info")
        .arg(mount_point)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut kv: HashMap<String, String> = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once(':') {
            kv.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    let device_id = kv
        .get("Device Node")
        .or_else(|| kv.get("Device Identifier"))
        .cloned()
        .unwrap_or_else(|| mount_point.display().to_string());

    let part_of_whole = kv
        .get("Part of Whole")
        .cloned()
        .unwrap_or_default();

    // For APFS synthesized containers, resolve the underlying physical store disk (e.g. disk4s2 -> disk4).
    let physical_whole_disk = kv
        .get("APFS Physical Store")
        .map(|s| {
            let trimmed = s.trim();
            if let Some(idx) = trimmed.find('s') {
                if trimmed[..idx].starts_with("disk") {
                    return trimmed[..idx].to_string();
                }
            }
            trimmed.to_string()
        })
        .filter(|s| s.starts_with("disk"))
        .unwrap_or_else(|| part_of_whole.clone());

    let volume_name = kv
        .get("Volume Name")
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("Not applicable (no file system)"))
        .cloned()
        .or_else(|| {
            mount_point
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "Macintosh HD".to_string());

    let filesystem = kv
        .get("File System Personality")
        .or_else(|| kv.get("Type (Bundle)"))
        .cloned()
        .unwrap_or_else(|| "Unknown".to_string());

    let protocol_raw = kv
        .get("Protocol")
        .cloned()
        .unwrap_or_else(|| "Unknown".to_string());

    let location = kv
        .get("Device Location")
        .cloned()
        .unwrap_or_default();
    let removable = kv
        .get("Removable Media")
        .cloned()
        .unwrap_or_default();
    let solid_state = kv
        .get("Solid State")
        .cloned()
        .unwrap_or_default();
    let read_only_str = kv
        .get("Read-Only Volume")
        .cloned()
        .unwrap_or_default();
    let media_name = kv
        .get("Device / Media Name")
        .cloned()
        .unwrap_or_default();
    let is_virtual = kv
        .get("Virtual")
        .map(|v| v.eq_ignore_ascii_case("Yes"))
        .unwrap_or(false);

    // Filter out macOS virtual recovery/preboot/TimeMachine snapshots unless it's root.
    if !is_root && (volume_name.starts_with("com.apple.") || protocol_raw.eq_ignore_ascii_case("Disk Image")) {
        // Still allow user-mounted DMGs only if writable; skip read-only system images
        if read_only_str.eq_ignore_ascii_case("Yes") {
            return None;
        }
    }

    let (stat_total, stat_free) = get_volume_space(mount_point).unwrap_or((0, 0));
    let disk_size_parsed = kv
        .get("Disk Size")
        .or_else(|| kv.get("Container Total Space"))
        .and_then(|s| extract_bytes_in_parens(s))
        .unwrap_or(stat_total);

    let total_bytes = if stat_total > 0 {
        stat_total
    } else {
        disk_size_parsed
    };
    let free_bytes = if stat_free > 0 {
        stat_free
    } else {
        kv.get("Volume Free Space")
            .or_else(|| kv.get("Container Free Space"))
            .and_then(|s| extract_bytes_in_parens(s))
            .unwrap_or(0)
    };

    let is_external = location.eq_ignore_ascii_case("External");
    let is_removable = removable.eq_ignore_ascii_case("Removable");
    let is_ssd = solid_state.eq_ignore_ascii_case("Yes");
    let proto_upper = protocol_raw.to_uppercase();

    let kind = if is_root {
        if is_ssd || proto_upper.contains("FABRIC") || proto_upper.contains("PCI") {
            DriveKind::InternalSsd
        } else {
            DriveKind::InternalHdd
        }
    } else if proto_upper.contains("SECURE DIGITAL") || media_name.to_uppercase().contains("SD ") || media_name.to_uppercase().contains("CARD READER") {
        DriveKind::SdCard
    } else if proto_upper.contains("USB") && is_removable {
        DriveKind::RemovableUsb
    } else if is_external || proto_upper.contains("USB") || proto_upper.contains("THUNDERBOLT") {
        DriveKind::ExternalDrive
    } else if is_virtual || proto_upper.contains("DISK IMAGE") {
        DriveKind::NetworkOrVirtual
    } else if is_ssd {
        DriveKind::InternalSsd
    } else {
        DriveKind::InternalHdd
    };

    // Match USB hardware info (VID/PID/Manufacturer/Serial/Speed) via physical whole-disk BSD name (e.g. "disk4")
    let usb_hw = usb_map
        .get(&physical_whole_disk)
        .or_else(|| usb_map.get(&part_of_whole))
        .or_else(|| usb_map.get(device_id.trim_start_matches("/dev/")))
        .or_else(|| {
            // Fallback: match by media name substring
            if !media_name.is_empty() {
                usb_map.values().find(|hw| {
                    hw.product_name
                        .as_deref()
                        .map(|p| media_name.contains(p) || p.contains(&media_name))
                        .unwrap_or(false)
                })
            } else {
                None
            }
        });

    let protocol = usb_hw
        .and_then(|h| h.bcd_usb.clone())
        .unwrap_or(protocol_raw);

    let vendor = usb_hw
        .and_then(|h| h.manufacturer.clone())
        .or_else(|| usb_hw.and_then(|h| h.scsi_vendor.clone()))
        .unwrap_or_default();
    let model = if !media_name.is_empty() {
        media_name
    } else {
        usb_hw
            .and_then(|h| h.product_name.clone())
            .unwrap_or_default()
    };

    // Expose raw whole-disk path for physical disks.
    // Note: write-capable raw LBA probing still strictly refuses `is_system` / `disk0`;
    // read-only HD Tune Pro Error Scan can safely read any physical disk.
    let raw_disk_path = if !is_virtual && physical_whole_disk.starts_with("disk") {
        Some(format!("/dev/r{}", physical_whole_disk))
    } else {
        None
    };

    Some(DiskInfo {
        name: volume_name,
        mount_point: mount_point.to_path_buf(),
        device_id,
        raw_disk_path,
        filesystem,
        total_bytes,
        free_bytes,
        physical_bytes: disk_size_parsed,
        protocol,
        kind,
        is_system: is_root,
        is_read_only: read_only_str.eq_ignore_ascii_case("Yes"),
        vendor,
        model,
        vid: usb_hw.and_then(|h| h.vid.clone()),
        pid: usb_hw.and_then(|h| h.pid.clone()),
        bcd_device: usb_hw.and_then(|h| h.bcd_device.clone()),
        scsi_vendor: usb_hw.and_then(|h| h.scsi_vendor.clone()),
        scsi_product: usb_hw.and_then(|h| h.scsi_product.clone()),
        scsi_rev: usb_hw.and_then(|h| h.scsi_rev.clone()),
        serial: usb_hw.and_then(|h| h.serial.clone()),
        link_speed: usb_hw.and_then(|h| h.speed.clone()),
        max_power_ma: usb_hw.and_then(|h| h.max_power_ma),
        usb_manufacturer: usb_hw.and_then(|h| h.manufacturer.clone()),
        usb_product: usb_hw.and_then(|h| h.product_name.clone()),
    })
}

/// Parses strings like "128.0 GB (128035676160 Bytes) (exactly ...)" -> Some(128035676160)
fn extract_bytes_in_parens(s: &str) -> Option<u64> {
    let start = s.find('(')?;
    let rest = &s[start + 1..];
    let end = rest.find(" Bytes")?;
    rest[..end].trim().parse::<u64>().ok()
}

/// Builds a lookup from BSD disk name (e.g. "disk4", "disk4s1") -> USB VID/PID/bcdDevice/bcdUSB/power/speed.
///
/// Primary source: `ioreg -r -c IOUSBHostDevice -l -w 0` (fast & works on ALL macOS 12..27+).
/// Secondary enrichment: `system_profiler SPUSBHostDataType -json` (macOS 15+) and `SPUSBDataType -json`
/// only when `ioreg` missed power draw (`max_power_ma`) or found no USB disks.
fn query_usb_hardware_map() -> HashMap<String, UsbHardwareInfo> {
    let mut map = HashMap::new();

    if let Ok(output) = Command::new("ioreg")
        .args(["-r", "-c", "IOUSBHostDevice", "-l", "-w", "0"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            parse_ioreg_usb_tree(&text, &mut map);
        }
    }

    let needs_enrichment =
        map.is_empty() || map.values().any(|hw| hw.max_power_ma.is_none() || hw.bcd_usb.is_none());
    if needs_enrichment {
        for data_type in ["SPUSBHostDataType", "SPUSBDataType"] {
            if let Ok(output) = Command::new("system_profiler")
                .args([data_type, "-json"])
                .output()
            {
                if output.status.success() {
                    if let Ok(json) = serde_json::from_slice::<Value>(&output.stdout) {
                        if let Some(buses) = json.get(data_type).and_then(|v| v.as_array()) {
                            for bus in buses {
                                walk_usb_tree(bus, &mut map);
                            }
                        }
                    }
                }
            }
            if !map.is_empty() && map.values().all(|hw| hw.max_power_ma.is_some()) {
                break;
            }
        }
    }

    map
}

/// Parses `ioreg -r -c IOUSBHostDevice -l -w 0` output by maintaining a nesting stack of
/// `IOUSBHostDevice` ancestors and flushing collected BSD names on frame pop so that EVERY
/// property anywhere inside the device subtree (including `bcdUSB`, `max_power_ma`, and SCSI
/// `Vendor/Product/Revision` appearing before or after `BSD Name`) attaches accurately.
fn parse_ioreg_usb_tree(text: &str, map: &mut HashMap<String, UsbHardwareInfo>) {
    let mut stack: Vec<(usize, UsbHardwareInfo, Vec<String>)> = Vec::new();
    let mut in_usb_device_props = false;

    let flush_frame = |hw: UsbHardwareInfo, bsds: Vec<String>, map: &mut HashMap<String, UsbHardwareInfo>| {
        if hw.vid.is_some() {
            for bsd in bsds {
                map.insert(bsd, hw.clone());
            }
        }
    };

    for line in text.lines() {
        if let Some(node_pos) = line.find("+-o ") {
            // Pop and flush any IOUSBHostDevice ancestors at same or deeper indent
            while let Some(&(top_indent, _, _)) = stack.last() {
                if top_indent >= node_pos {
                    if let Some((_, hw, bsds)) = stack.pop() {
                        flush_frame(hw, bsds, map);
                    }
                } else {
                    break;
                }
            }

            if line.contains("<class IOUSBHostDevice,")
                || line.contains("<class IOUSBDevice,")
                || line.contains("<class AppleUSBHostDevice,")
            {
                let name_raw = &line[node_pos + 4..];
                let clean_name = name_raw
                    .split('@')
                    .next()
                    .unwrap_or(name_raw)
                    .split("<class")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let mut hw = UsbHardwareInfo::default();
                if !clean_name.is_empty() {
                    hw.product_name = Some(clean_name);
                }
                stack.push((node_pos, hw, Vec::new()));
                in_usb_device_props = true;
            } else {
                in_usb_device_props = false;
            }
            continue;
        }

        let Some((_, cur_hw, cur_bsds)) = stack.last_mut() else {
            continue;
        };

        let trimmed = line.trim_start_matches(|c: char| c == ' ' || c == '|');
        if !trimmed.starts_with('"') {
            continue;
        }
        let Some((k_part, v_part)) = trimmed.split_once(" = ") else {
            continue;
        };
        let key = k_part.trim().trim_matches('"');
        let val = v_part.trim();

        match key {
            "BSD Name" => {
                let bsd = val.trim_matches('"').trim();
                if bsd.starts_with("disk") && !cur_bsds.iter().any(|b| b == bsd) {
                    cur_bsds.push(bsd.to_string());
                }
            }
            "idVendor" if in_usb_device_props || cur_hw.vid.is_none() => {
                if let Some(n) = parse_u64_dec_or_hex(val) {
                    cur_hw.vid = Some(format!("0x{:04X}", n));
                }
            }
            "idProduct" if in_usb_device_props || cur_hw.pid.is_none() => {
                if let Some(n) = parse_u64_dec_or_hex(val) {
                    cur_hw.pid = Some(format!("0x{:04X}", n));
                }
            }
            "bcdDevice" if in_usb_device_props || cur_hw.bcd_device.is_none() => {
                if let Some(n) = parse_u64_dec_or_hex(val) {
                    cur_hw.bcd_device = Some(format!("{:04X}", n & 0xFFFF));
                }
            }
            "bcdUSB" if in_usb_device_props || cur_hw.bcd_usb.is_none() => {
                if let Some(n) = parse_u64_dec_or_hex(val) {
                    let bcd = (n & 0xFFFF) as u16;
                    if bcd > 0 {
                        let major = (bcd >> 8) & 0xFF;
                        let minor = bcd & 0xFF;
                        cur_hw.bcd_usb_raw = Some(bcd);
                        cur_hw.bcd_usb = Some(format!("USB {:X}.{:02X}", major, minor));
                    }
                }
            }
            "kUSBCurrent" | "USB Current" | "Current Required" | "Bus Power Used"
                if in_usb_device_props || cur_hw.max_power_ma.is_none() =>
            {
                if let Some(ma) = parse_power_ma_str(val) {
                    cur_hw.max_power_ma = Some(ma);
                }
            }
            "bMaxPower" | "MaxPower" | "kUSBMaxPower"
                if in_usb_device_props || cur_hw.max_power_ma.is_none() =>
            {
                if let Some(raw) = parse_u64_dec_or_hex(val) {
                    let raw_u32 = raw as u32;
                    if raw_u32 > 0 {
                        let is_usb3 = cur_hw.bcd_usb_raw.unwrap_or(0) >= 0x0300;
                        let ma = if raw_u32 <= 255 {
                            raw_u32 * if is_usb3 { 8 } else { 2 }
                        } else {
                            raw_u32
                        };
                        cur_hw.max_power_ma = Some(ma);
                    }
                }
            }
            "USB Vendor Name" | "kUSBVendorString"
                if in_usb_device_props || cur_hw.manufacturer.is_none() =>
            {
                let s = val.trim_matches('"').trim();
                if !s.is_empty() {
                    cur_hw.manufacturer = Some(s.to_string());
                }
            }
            "USB Product Name" | "kUSBProductString" if in_usb_device_props => {
                let s = val.trim_matches('"').trim();
                if !s.is_empty() {
                    cur_hw.product_name = Some(s.to_string());
                }
            }
            "USB Serial Number" | "kUSBSerialNumberString"
                if in_usb_device_props || cur_hw.serial.is_none() =>
            {
                let s = val.trim_matches('"').trim();
                if !s.is_empty() && !s.eq_ignore_ascii_case("Not Provided") {
                    cur_hw.serial = Some(s.to_string());
                }
            }
            "USBSpeed" if in_usb_device_props || cur_hw.speed.is_none() => {
                if let Some(n) = parse_u64_dec_or_hex(val) {
                    cur_hw.speed = Some(humanize_ioreg_usb_speed(n));
                }
            }
            "Device Speed" if in_usb_device_props && cur_hw.speed.is_none() => {
                if let Some(n) = parse_u64_dec_or_hex(val) {
                    cur_hw.speed = Some(humanize_legacy_device_speed(n));
                }
            }
            "Vendor Identification" => {
                let s = val.trim_matches('"').trim();
                if !s.is_empty() {
                    cur_hw.scsi_vendor = Some(s.to_string());
                }
            }
            "Product Identification" => {
                let s = val.trim_matches('"').trim();
                if !s.is_empty() {
                    cur_hw.scsi_product = Some(s.to_string());
                }
            }
            "Product Revision Level" => {
                let s = val.trim_matches('"').trim();
                if !s.is_empty() {
                    cur_hw.scsi_rev = Some(s.to_string());
                }
            }
            _ => {}
        }
    }

    // Flush any remaining active device frames on the stack
    while let Some((_, hw, bsds)) = stack.pop() {
        flush_frame(hw, bsds, map);
    }
}

fn parse_u64_dec_or_hex(raw: &str) -> Option<u64> {
    let s = raw.trim().trim_matches('"');
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

/// Parses `"144 mA"`, `"500"`, `"0x90"` -> `Some(144)` / `Some(500)`.
fn parse_power_ma_str(raw: &str) -> Option<u32> {
    let s = raw.trim().trim_matches('"');
    let token = s.split_whitespace().next().unwrap_or(s);
    let n = parse_u64_dec_or_hex(token)? as u32;
    if n > 0 && n <= 5000 {
        Some(n)
    } else {
        None
    }
}

fn humanize_ioreg_usb_speed(speed_code: u64) -> String {
    match speed_code {
        0 | 1 => "低速 (LowSpeed 1.5Mbps)".to_string(),
        2 => "全速 (FullSpeed 12Mbps)".to_string(),
        3 => "高速 (HighSpeed 480Mbps)".to_string(),
        4 => "超高速 (SuperSpeed 5Gbps)".to_string(),
        5 => "超高速+ (SuperSpeed+ 10Gbps)".to_string(),
        6 => "超高速+ (USB 3.2 Gen 2x2 20Gbps)".to_string(),
        7 => "USB4 (40Gbps)".to_string(),
        other => format!("USB (Speed {})", other),
    }
}

fn humanize_legacy_device_speed(speed_code: u64) -> String {
    match speed_code {
        0 => "低速 (LowSpeed 1.5Mbps)".to_string(),
        1 => "全速 (FullSpeed 12Mbps)".to_string(),
        2 => "高速 (HighSpeed 480Mbps)".to_string(),
        3 => "超高速 (SuperSpeed 5Gbps)".to_string(),
        4 => "超高速+ (SuperSpeed+ 10Gbps)".to_string(),
        other => format!("USB (Speed {})", other),
    }
}

fn normalize_sp_hex4(raw: &str) -> Option<String> {
    let tok = raw.split_whitespace().next().unwrap_or(raw).trim();
    let hex = tok
        .strip_prefix("0x")
        .or_else(|| tok.strip_prefix("0X"))
        .unwrap_or(tok);
    if hex.is_empty() || hex.len() > 4 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let val = u16::from_str_radix(hex, 16).ok()?;
    Some(format!("0x{:04X}", val))
}

fn normalize_sp_bcd_device(raw: &str) -> Option<String> {
    let s = raw.trim();
    if let Some((maj, min)) = s.split_once('.') {
        let m1 = u8::from_str_radix(maj.trim(), 16).ok()?;
        let m2 = u8::from_str_radix(min.trim(), 16).ok()?;
        return Some(format!("{:02X}{:02X}", m1, m2));
    }
    if s.len() == 4 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(s.to_ascii_uppercase());
    }
    None
}

fn walk_usb_tree(node: &Value, map: &mut HashMap<String, UsbHardwareInfo>) {
    let vid = node
        .get("vendor_id")
        .and_then(|v| v.as_str())
        .and_then(normalize_sp_hex4);
    let pid = node
        .get("product_id")
        .and_then(|v| v.as_str())
        .and_then(normalize_sp_hex4);
    let serial = node
        .get("serial_num")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let max_power_ma = node
        .get("bus_power_used")
        .or_else(|| node.get("current_required"))
        .or_else(|| node.get("extra_operating_current"))
        .and_then(|v| {
            v.as_u64()
                .map(|n| n as u32)
                .or_else(|| v.as_str().and_then(parse_power_ma_str))
        })
        .filter(|&ma| ma > 0 && ma <= 5000);

    let hw = UsbHardwareInfo {
        vid: vid.clone(),
        pid: pid.clone(),
        bcd_device: node
            .get("bcd_device")
            .and_then(|v| v.as_str())
            .and_then(normalize_sp_bcd_device),
        bcd_usb: None,
        bcd_usb_raw: None,
        max_power_ma,
        manufacturer: node
            .get("manufacturer")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        serial: serial.clone(),
        speed: node
            .get("device_speed")
            .and_then(|v| v.as_str())
            .map(humanize_usb_speed),
        product_name: node
            .get("_name")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        scsi_vendor: None,
        scsi_product: None,
        scsi_rev: None,
    };

    // Enrich existing ioreg entries matching this device's VID+PID (+ optional serial) with missing power/speed
    if vid.is_some() && pid.is_some() {
        for existing in map.values_mut() {
            let vp_match = existing.vid == vid && existing.pid == pid;
            let sn_match = serial.is_none()
                || existing.serial.is_none()
                || existing.serial == serial;
            if vp_match && sn_match {
                if existing.max_power_ma.is_none() {
                    existing.max_power_ma = hw.max_power_ma;
                }
                if existing.speed.is_none() {
                    existing.speed = hw.speed.clone();
                }
                if existing.bcd_device.is_none() {
                    existing.bcd_device = hw.bcd_device.clone();
                }
                if existing.manufacturer.is_none() {
                    existing.manufacturer = hw.manufacturer.clone();
                }
            }
        }
    }

    // If this USB node has `Media` entries, map each BSD name (e.g. "disk4") to its USB hardware info.
    if let Some(media_list) = node.get("Media").and_then(|v| v.as_array()) {
        for m in media_list {
            if let Some(bsd) = m.get("bsd_name").and_then(|v| v.as_str()) {
                map.entry(bsd.to_string()).or_insert_with(|| hw.clone());
            }
            if let Some(vols) = m.get("volumes").and_then(|v| v.as_array()) {
                for vol in vols {
                    if let Some(bsd) = vol.get("bsd_name").and_then(|v| v.as_str()) {
                        map.entry(bsd.to_string()).or_insert_with(|| hw.clone());
                    }
                }
            }
        }
    }

    if let Some(children) = node.get("_items").and_then(|v| v.as_array()) {
        for child in children {
            walk_usb_tree(child, map);
        }
    }
}

fn humanize_usb_speed(raw: &str) -> String {
    match raw {
        "super_speed_plus" => "超高速+ (SuperSpeed+ 10Gbps)".to_string(),
        "super_speed" => "超高速 (SuperSpeed 5Gbps)".to_string(),
        "high_speed" => "高速 (HighSpeed 480Mbps)".to_string(),
        "full_speed" => "全速 (FullSpeed 12Mbps)".to_string(),
        "low_speed" => "低速 (LowSpeed 1.5Mbps)".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioreg_parser_maps_nested_usb_flash_drive_not_parent_hub() {
        let sample = r#"
+-o USB3 Gen2 Hub@02200000  <class IOUSBHostDevice, id 0x100000a33, registered, matched, active>
  |   "USB Vendor Name" = "Apple"
  |   "USBSpeed" = 5
  |   "idVendor" = 1452
  |   "idProduct" = 32780
  |   "bcdDevice" = 22037
  +-o AppleUSB30Hub@02200000  <class AppleUSB30Hub, id 0x100000a3c>
    +-o usb3-hub-port1@02210000  <class AppleUSB30HubPort, id 0x100000a3f>
      +-o USB3.0 Hub@02210000  <class IOUSBHostDevice, id 0x1000dbe75>
        |   "USB Vendor Name" = "VIA Labs, Inc."
        |   "USBSpeed" = 4
        |   "idVendor" = 8457
        |   "idProduct" = 2067
        |   "bcdDevice" = 36885
        +-o AppleUSB30Hub@02210000  <class AppleUSB30Hub, id 0x1000dbe77>
          +-o AppleUSB30HubPort@02211000  <class AppleUSB30HubPort, id 0x1000dbe7b>
            +-o Flash Disk@02211000  <class IOUSBHostDevice, id 0x1000f0001>
              |   "USB Vendor Name" = "Kingston"
              |   "USB Product Name" = "DataTraveler 3.0"
              |   "USB Serial Number" = "E0D55EA574D51891D8750356"
              |   "USBSpeed" = 4
              |   "idVendor" = 2385
              |   "idProduct" = 5734
              |   "bcdDevice" = 272
              |   "bcdUSB" = 800
              |   "bMaxPower" = 18
              +-o IOUSBHostInterface@0  <class IOUSBHostInterface, id 0x1000f0002>
                +-o IOUSBMassStorageDriver  <class IOUSBMassStorageDriver, id 0x1000f0003>
                  +-o Generic Flash Disk Media  <class IOMedia, id 0x1000f0004>
                    |   "BSD Name" = "disk4"
                    +-o FDisk_partition_scheme  <class IOFDiskPartitionScheme, id 0x1000f0005>
                      +-o DOS_FAT_32_Untitled_1@1  <class IOMedia, id 0x1000f0006>
                        |   "BSD Name" = "disk4s1"
                  +-o IOSCSIPeripheralDeviceNub  <class IOSCSIPeripheralDeviceNub, id 0x1000f0007>
                    |   "Vendor Identification" = "Kingston"
                    |   "Product Identification" = "DataTraveler 3.0"
                    |   "Product Revision Level" = "0000"
"#;
        let mut map = HashMap::new();
        parse_ioreg_usb_tree(sample, &mut map);

        let d4 = map.get("disk4").expect("disk4 mapped");
        assert_eq!(d4.vid.as_deref(), Some("0x0951"));
        assert_eq!(d4.pid.as_deref(), Some("0x1666"));
        assert_eq!(d4.bcd_device.as_deref(), Some("0110"));
        assert_eq!(d4.bcd_usb.as_deref(), Some("USB 3.20"));
        assert_eq!(d4.max_power_ma, Some(144));
        assert_eq!(d4.manufacturer.as_deref(), Some("Kingston"));
        assert_eq!(d4.product_name.as_deref(), Some("DataTraveler 3.0"));
        assert_eq!(d4.serial.as_deref(), Some("E0D55EA574D51891D8750356"));
        assert_eq!(d4.speed.as_deref(), Some("超高速 (SuperSpeed 5Gbps)"));
        // Even though SCSI properties appeared AFTER BSD Name, frame-pop flushing attached them!
        assert_eq!(d4.scsi_vendor.as_deref(), Some("Kingston"));
        assert_eq!(d4.scsi_rev.as_deref(), Some("0000"));

        let d4s1 = map.get("disk4s1").expect("disk4s1 mapped");
        assert_eq!(d4s1.vid.as_deref(), Some("0x0951"));
        assert_eq!(d4s1.bcd_usb.as_deref(), Some("USB 3.20"));
        assert_eq!(d4s1.max_power_ma, Some(144));
    }
}
