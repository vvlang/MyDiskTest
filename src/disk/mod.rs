pub mod direct_io;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriveKind {
    RemovableUsb,
    ExternalDrive,
    SdCard,
    InternalSsd,
    InternalHdd,
    NetworkOrVirtual,
    Unknown,
}

impl DriveKind {
    pub fn label_zh(&self) -> &'static str {
        match self {
            DriveKind::RemovableUsb => "U 盘 / 移动闪存",
            DriveKind::ExternalDrive => "移动硬盘 / 外置存储",
            DriveKind::SdCard => "SD / TF 存储卡",
            DriveKind::InternalSsd => "内置固态硬盘",
            DriveKind::InternalHdd => "内置机械硬盘",
            DriveKind::NetworkOrVirtual => "虚拟 / 网络磁盘",
            DriveKind::Unknown => "未知存储设备",
        }
    }

    pub fn is_external_or_removable(&self) -> bool {
        matches!(
            self,
            DriveKind::RemovableUsb | DriveKind::ExternalDrive | DriveKind::SdCard
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskInfo {
    /// Display name, e.g. "SANDISK 128G" or "E:\"
    pub name: String,
    /// Mount path where safe test directory can be created, e.g. "/Volumes/USB" or "E:\"
    pub mount_point: PathBuf,
    /// OS device identifier, e.g. "/dev/disk4s1" or "\\.\PhysicalDrive1"
    pub device_id: String,
    /// Raw physical whole-disk path for ultra-fast LBA probing (e.g. "/dev/rdisk4" or "\\.\PhysicalDrive1")
    pub raw_disk_path: Option<String>,
    /// Filesystem name, e.g. "ExFAT", "FAT32", "NTFS", "APFS"
    pub filesystem: String,
    /// Total volume capacity in bytes
    pub total_bytes: u64,
    /// Available free space in bytes
    pub free_bytes: u64,
    /// Physical disk reported total bytes (may be larger than partition)
    pub physical_bytes: u64,
    /// Connection protocol / bus, e.g. "USB 3.0 (5 Gbps)", "USB 2.0 (480 Mbps)", "NVMe", "Thunderbolt"
    pub protocol: String,
    /// Drive category
    pub kind: DriveKind,
    /// True if this is the OS boot / system drive (protected by default)
    pub is_system: bool,
    /// True if mounted read-only
    pub is_read_only: bool,
    /// Hardware vendor / manufacturer if detected
    pub vendor: String,
    /// Hardware model / media name
    pub model: String,
    /// USB Vendor ID (e.g. "0x0781")
    pub vid: Option<String>,
    /// USB Product ID (e.g. "0x5583")
    pub pid: Option<String>,
    /// Serial number if available
    pub serial: Option<String>,
    /// USB link speed description if available
    pub link_speed: Option<String>,
}

impl DiskInfo {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    pub fn used_ratio(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.used_bytes() as f32 / self.total_bytes as f32
        }
    }
}

/// Scans all mounted volumes and enriches removable/external drives with hardware metadata.
pub fn scan_disks() -> Vec<DiskInfo> {
    let mut disks = scan_disks_platform();
    // Sort: external/removable drives first, then non-system internal, then system drive last.
    disks.sort_by(|a, b| {
        let rank = |d: &DiskInfo| -> u8 {
            if d.is_system {
                3
            } else if d.kind.is_external_or_removable() {
                0
            } else if matches!(d.kind, DriveKind::NetworkOrVirtual) {
                2
            } else {
                1
            }
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.mount_point.cmp(&b.mount_point))
    });
    disks
}

#[cfg(target_os = "macos")]
fn scan_disks_platform() -> Vec<DiskInfo> {
    macos::scan_disks_macos()
}

#[cfg(target_os = "windows")]
fn scan_disks_platform() -> Vec<DiskInfo> {
    windows::scan_disks_windows()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn scan_disks_platform() -> Vec<DiskInfo> {
    vec![DiskInfo {
        name: "Root".to_string(),
        mount_point: PathBuf::from("/"),
        device_id: "/dev/root".to_string(),
        raw_disk_path: None,
        filesystem: "ext4".to_string(),
        total_bytes: 0,
        free_bytes: 0,
        physical_bytes: 0,
        protocol: "Unknown".to_string(),
        kind: DriveKind::InternalSsd,
        is_system: true,
        is_read_only: false,
        vendor: String::new(),
        model: String::new(),
        vid: None,
        pid: None,
        serial: None,
        link_speed: None,
    }]
}

/// Builds a synthetic DiskInfo for a user-selected custom folder (useful for testing or NAS mounts).
pub fn disk_info_for_custom_path(path: PathBuf) -> DiskInfo {
    let (total_bytes, free_bytes) = direct_io::get_volume_space(&path).unwrap_or((0, 0));
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| format!("自定义目录: {}", s))
        .unwrap_or_else(|| format!("自定义目录: {}", path.display()));
    DiskInfo {
        name,
        mount_point: path.clone(),
        device_id: path.display().to_string(),
        raw_disk_path: None,
        filesystem: "自定义路径".to_string(),
        total_bytes,
        free_bytes,
        physical_bytes: total_bytes,
        protocol: "Custom Path".to_string(),
        kind: DriveKind::ExternalDrive,
        is_system: false,
        is_read_only: false,
        vendor: "用户指定".to_string(),
        model: path.display().to_string(),
        vid: None,
        pid: None,
        serial: None,
        link_speed: None,
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= TIB {
        format!("{:.2} TiB", b / TIB)
    } else if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_bytes_decimal(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    const TB: f64 = 1_000_000_000_000.0;

    let b = bytes as f64;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}
