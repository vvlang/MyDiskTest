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
    manufacturer: Option<String>,
    serial: Option<String>,
    speed: Option<String>,
    product_name: Option<String>,
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

    // Match USB hardware info (VID/PID/Manufacturer/Serial/Speed) via whole-disk BSD name (e.g. "disk4")
    let usb_hw = usb_map
        .get(&part_of_whole)
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

    let mut protocol = protocol_raw;
    if let Some(hw) = usb_hw {
        if let Some(ref spd) = hw.speed {
            protocol = format!("{} ({})", protocol, spd);
        }
    }

    let vendor = usb_hw
        .and_then(|h| h.manufacturer.clone())
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
        serial: usb_hw.and_then(|h| h.serial.clone()),
        link_speed: usb_hw.and_then(|h| h.speed.clone()),
    })
}

/// Parses strings like "128.0 GB (128035676160 Bytes) (exactly ...)" -> Some(128035676160)
fn extract_bytes_in_parens(s: &str) -> Option<u64> {
    let start = s.find('(')?;
    let rest = &s[start + 1..];
    let end = rest.find(" Bytes")?;
    rest[..end].trim().parse::<u64>().ok()
}

/// Queries `system_profiler SPUSBDataType -json` once to build a lookup from BSD disk name -> USB VID/PID/speed.
fn query_usb_hardware_map() -> HashMap<String, UsbHardwareInfo> {
    let mut map = HashMap::new();
    let output = match Command::new("system_profiler")
        .args(["SPUSBDataType", "-json"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return map,
    };

    let json: Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return map,
    };

    if let Some(buses) = json.get("SPUSBDataType").and_then(|v| v.as_array()) {
        for bus in buses {
            walk_usb_tree(bus, &mut map);
        }
    }
    map
}

fn walk_usb_tree(node: &Value, map: &mut HashMap<String, UsbHardwareInfo>) {
    let hw = UsbHardwareInfo {
        vid: node
            .get("vendor_id")
            .and_then(|v| v.as_str())
            .map(|s| s.split_whitespace().next().unwrap_or(s).to_string()),
        pid: node
            .get("product_id")
            .and_then(|v| v.as_str())
            .map(|s| s.split_whitespace().next().unwrap_or(s).to_string()),
        bcd_device: node
            .get("bcd_device")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        manufacturer: node
            .get("manufacturer")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        serial: node
            .get("serial_num")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        speed: node
            .get("device_speed")
            .and_then(|v| v.as_str())
            .map(humanize_usb_speed),
        product_name: node
            .get("_name")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
    };

    // If this USB node has `Media` entries, map each BSD name (e.g. "disk4") to its USB hardware info.
    if let Some(media_list) = node.get("Media").and_then(|v| v.as_array()) {
        for m in media_list {
            if let Some(bsd) = m.get("bsd_name").and_then(|v| v.as_str()) {
                map.insert(bsd.to_string(), hw.clone());
            }
            if let Some(vols) = m.get("volumes").and_then(|v| v.as_array()) {
                for vol in vols {
                    if let Some(bsd) = vol.get("bsd_name").and_then(|v| v.as_str()) {
                        map.insert(bsd.to_string(), hw.clone());
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
        "super_speed_plus" => "USB 3.2 Gen 2 (10 Gbps)".to_string(),
        "super_speed" => "USB 3.0 (5 Gbps)".to_string(),
        "high_speed" => "USB 2.0 (480 Mbps)".to_string(),
        "full_speed" => "USB 1.1 (12 Mbps)".to_string(),
        "low_speed" => "USB 1.0 (1.5 Mbps)".to_string(),
        other => other.to_string(),
    }
}
