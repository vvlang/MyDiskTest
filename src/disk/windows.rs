use crate::disk::direct_io::get_volume_space;
use crate::disk::{DiskInfo, DriveKind};
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;
const FILE_READ_ONLY_VOLUME: u32 = 0x00080000;
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Deserialize, Default)]
struct WinDiskMeta {
    #[serde(rename = "DriveLetter")]
    drive_letter: Option<String>,
    #[serde(rename = "Model")]
    model: Option<String>,
    #[serde(rename = "BusType")]
    bus_type: Option<String>,
    #[serde(rename = "SerialNumber")]
    serial_number: Option<String>,
    #[serde(rename = "Size")]
    size: Option<u64>,
    #[serde(rename = "MediaType")]
    media_type: Option<String>,
    #[serde(rename = "PnpId")]
    pnp_id: Option<String>,
    #[serde(rename = "FirmwareVersion")]
    firmware_version: Option<String>,
    #[serde(rename = "DiskNumber")]
    disk_number: Option<u32>,
}

pub fn scan_disks_windows() -> Vec<DiskInfo> {
    let sys_drive = std::env::var("SystemDrive")
        .unwrap_or_else(|_| "C:".to_string())
        .to_uppercase();

    let meta_map = query_windows_disk_metadata();
    let bitmask = unsafe { GetLogicalDrives() };
    let mut results = Vec::new();

    for i in 0..26u8 {
        if (bitmask & (1 << i)) == 0 {
            continue;
        }
        let letter = (b'A' + i) as char;
        let root_str = format!("{}:\\", letter);
        let wide_root: Vec<u16> = OsString::from(&root_str)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let drive_type = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
        if drive_type == DRIVE_CDROM || drive_type <= 1 {
            continue;
        }

        let mount_point = PathBuf::from(&root_str);
        let (total_bytes, free_bytes) = match get_volume_space(&mount_point) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut vol_name_buf = [0u16; 260];
        let mut fs_name_buf = [0u16; 64];
        let mut fs_flags: u32 = 0;
        let ok = unsafe {
            GetVolumeInformationW(
                wide_root.as_ptr(),
                vol_name_buf.as_mut_ptr(),
                vol_name_buf.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut fs_flags,
                fs_name_buf.as_mut_ptr(),
                fs_name_buf.len() as u32,
            )
        };

        let vol_name = if ok != 0 {
            utf16_z_to_string(&vol_name_buf)
        } else {
            String::new()
        };
        let filesystem = if ok != 0 {
            utf16_z_to_string(&fs_name_buf)
        } else {
            "Unknown".to_string()
        };
        let is_read_only = (fs_flags & FILE_READ_ONLY_VOLUME) != 0;

        let letter_key = letter.to_string().to_uppercase();
        let meta = meta_map.get(&letter_key);

        let bus_type = meta
            .and_then(|m| m.bus_type.clone())
            .unwrap_or_else(|| match drive_type {
                DRIVE_REMOVABLE => "USB".to_string(),
                DRIVE_REMOTE => "Network".to_string(),
                DRIVE_RAMDISK => "RAMDisk".to_string(),
                _ => "Local".to_string(),
            });

        let model = meta.and_then(|m| m.model.clone()).unwrap_or_default();
        let serial = meta
            .and_then(|m| m.serial_number.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let physical_bytes = meta.and_then(|m| m.size).unwrap_or(total_bytes);
        let device_id = meta
            .and_then(|m| m.disk_number)
            .map(|n| format!(r"\\.\PhysicalDrive{} ({}:)", n, letter))
            .unwrap_or_else(|| format!("{}:", letter));

        let (vid, pid) = meta
            .and_then(|m| m.pnp_id.as_deref())
            .map(parse_vid_pid_from_pnp)
            .unwrap_or((None, None));

        let is_system = format!("{}:", letter).eq_ignore_ascii_case(&sys_drive);
        let bus_upper = bus_type.to_uppercase();
        let media_upper = meta
            .and_then(|m| m.media_type.as_deref())
            .unwrap_or("")
            .to_uppercase();

        let kind = if drive_type == DRIVE_REMOTE || drive_type == DRIVE_RAMDISK {
            DriveKind::NetworkOrVirtual
        } else if bus_upper.contains("SD") || bus_upper.contains("MMC") {
            DriveKind::SdCard
        } else if drive_type == DRIVE_REMOVABLE {
            DriveKind::RemovableUsb
        } else if bus_upper.contains("USB") {
            DriveKind::ExternalDrive
        } else if media_upper.contains("SSD") || bus_upper.contains("NVME") {
            DriveKind::InternalSsd
        } else if media_upper.contains("HDD") {
            DriveKind::InternalHdd
        } else if is_system {
            DriveKind::InternalSsd
        } else {
            DriveKind::Unknown
        };

        let display_name = if vol_name.is_empty() {
            format!("本地磁盘 ({}:)", letter)
        } else {
            format!("{} ({}:)", vol_name, letter)
        };

        let raw_disk_path = meta
            .and_then(|m| m.disk_number)
            .map(|n| format!(r"\\.\PhysicalDrive{}", n));

        let bcd_device = meta
            .and_then(|m| m.firmware_version.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        results.push(DiskInfo {
            name: display_name,
            mount_point,
            device_id,
            raw_disk_path,
            filesystem,
            total_bytes,
            free_bytes,
            physical_bytes,
            protocol: bus_type,
            kind,
            is_system,
            is_read_only,
            vendor: String::new(),
            model,
            vid,
            pid,
            bcd_device,
            serial,
            link_speed: None,
        });
    }

    results
}

fn utf16_z_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..len]).to_string_lossy().into_owned()
}

fn parse_vid_pid_from_pnp(pnp: &str) -> (Option<String>, Option<String>) {
    let upper = pnp.to_uppercase();
    let extract_hex4 = |tag: &str| -> Option<String> {
        let idx = upper.find(tag)?;
        let rest = &upper[idx + tag.len()..];
        let hex: String = rest.chars().take(4).collect();
        if hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(format!("0x{}", hex))
        } else {
            None
        }
    };
    (extract_hex4("VID_"), extract_hex4("PID_"))
}

fn query_windows_disk_metadata() -> HashMap<String, WinDiskMeta> {
    let mut map = HashMap::new();
    let ps_script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$out = @()
Get-Partition | Where-Object { $_.DriveLetter } | ForEach-Object {
    $part = $_
    $disk = Get-Disk -Number $part.DiskNumber
    $phys = Get-PhysicalDisk | Where-Object { $_.DeviceId -eq [string]$part.DiskNumber } | Select-Object -First 1
    $wmi = Get-CimInstance Win32_DiskDrive | Where-Object { $_.Index -eq $part.DiskNumber } | Select-Object -First 1
    $out += [PSCustomObject]@{
        DriveLetter  = [string]$part.DriveLetter
        DiskNumber   = [uint32]$part.DiskNumber
        Model        = $disk.FriendlyName
        BusType      = [string]$disk.BusType
        SerialNumber = $disk.SerialNumber
        Size         = [uint64]$disk.Size
        MediaType    = if ($phys) { [string]$phys.MediaType } else { "" }
        PnpId        = if ($wmi) { [string]$wmi.PNPDeviceID } else { "" }
        FirmwareVersion = if ($wmi) { [string]$wmi.FirmwareRevision } else { [string]$disk.FirmwareVersion }
    }
}
$out | ConvertTo-Json -Compress
"#;

    let output = match Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps_script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return map,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return map;
    }

    // PowerShell ConvertTo-Json outputs a single object when count == 1, or an array when count > 1.
    if trimmed.starts_with('[') {
        if let Ok(list) = serde_json::from_str::<Vec<WinDiskMeta>>(trimmed) {
            for item in list {
                if let Some(ref letter) = item.drive_letter {
                    map.insert(letter.to_uppercase(), item);
                }
            }
        }
    } else if trimmed.starts_with('{') {
        if let Ok(item) = serde_json::from_str::<WinDiskMeta>(trimmed) {
            if let Some(ref letter) = item.drive_letter {
                map.insert(letter.to_uppercase(), item);
            }
        }
    }

    map
}
