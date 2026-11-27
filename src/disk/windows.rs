use crate::disk::direct_io::get_volume_space;
use crate::disk::{DiskInfo, DriveKind};
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

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
    #[serde(rename = "UsbPnpId")]
    usb_pnp_id: Option<String>,
    #[serde(rename = "UsbHwId")]
    usb_hw_id: Option<String>,
    #[serde(rename = "UsbVendor")]
    usb_vendor: Option<String>,
    #[serde(rename = "FirmwareVersion")]
    firmware_version: Option<String>,
    #[serde(rename = "DiskNumber")]
    disk_number: Option<u32>,
    #[serde(rename = "UsbPortNumber")]
    usb_port_number: Option<u32>,
    #[serde(rename = "UsbHubId")]
    usb_hub_id: Option<String>,
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

        // Extract VID/PID from walked parent USB\VID_xxxx&PID_xxxx first, falling back to DiskDrive PNPDeviceID
        let (vid, pid) = meta
            .and_then(|m| {
                m.usb_pnp_id
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or(m.usb_hw_id.as_deref().filter(|s| !s.is_empty()))
                    .or(m.pnp_id.as_deref())
            })
            .map(parse_vid_pid_from_pnp)
            .unwrap_or((None, None));

        // Query native Windows USB Hub descriptors (USB 3.00 / HighSpeed / MaxPower mA / iManufacturer / iProduct)
        let hub_desc = meta.and_then(|m| {
            let hub_id = m.usb_hub_id.as_deref().filter(|s| !s.is_empty())?;
            let port = m.usb_port_number.filter(|&p| p > 0)?;
            query_usb_hub_descriptors(hub_id, port)
        });

        // Extract USB bcdDevice (prefer live USB descriptor "0A00", then HardwareId REV_xxxx, then FirmwareVersion)
        let bcd_device = hub_desc
            .as_ref()
            .and_then(|h| h.bcd_device.clone())
            .or_else(|| {
                meta.and_then(|m| {
                    m.usb_hw_id
                        .as_deref()
                        .and_then(parse_rev_from_hw_id)
                        .or_else(|| {
                            m.firmware_version
                                .as_deref()
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                        })
                })
            });

        let (scsi_vendor, scsi_product, scsi_rev) = meta
            .and_then(|m| m.pnp_id.as_deref())
            .map(parse_usbstor_ven_prod_rev)
            .unwrap_or((None, None, None));

        let vendor = hub_desc
            .as_ref()
            .and_then(|h| h.manufacturer.clone())
            .or_else(|| {
                meta.and_then(|m| m.usb_vendor.clone())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| scsi_vendor.clone())
            .unwrap_or_default();

        let protocol = hub_desc
            .as_ref()
            .and_then(|h| h.usb_version.clone())
            .unwrap_or(bus_type.clone());
        let link_speed = hub_desc.as_ref().and_then(|h| h.speed_label.clone());
        let max_power_ma = hub_desc.as_ref().and_then(|h| h.max_power_ma);
        let usb_manufacturer = hub_desc.as_ref().and_then(|h| h.manufacturer.clone());
        let usb_product = hub_desc.as_ref().and_then(|h| h.product.clone());

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

        results.push(DiskInfo {
            name: display_name,
            mount_point,
            device_id,
            raw_disk_path,
            filesystem,
            total_bytes,
            free_bytes,
            physical_bytes,
            protocol,
            kind,
            is_system,
            is_read_only,
            vendor,
            model,
            vid,
            pid,
            bcd_device,
            scsi_vendor,
            scsi_product,
            scsi_rev,
            serial,
            link_speed,
            max_power_ma,
            usb_manufacturer,
            usb_product,
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

fn parse_rev_from_hw_id(hw: &str) -> Option<String> {
    let upper = hw.to_uppercase();
    let idx = upper.find("REV_")?;
    let rest = &upper[idx + 4..];
    let hex: String = rest.chars().take(4).collect();
    if hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        // Convert BCD "0100" -> "1.00", "1100" -> "11.00"
        let major = hex[0..2].trim_start_matches('0');
        let major = if major.is_empty() { "0" } else { major };
        Some(format!("{}.{}", major, &hex[2..4]))
    } else {
        None
    }
}

/// Parses `USBSTOR\DISK&VEN_SANDISK&PROD_ULTRA&REV_1.00\...` or `SCSI\DISK&VEN_...`.
fn parse_usbstor_ven_prod_rev(pnp: &str) -> (Option<String>, Option<String>, Option<String>) {
    let extract = |tag: &str| -> Option<String> {
        let upper = pnp.to_uppercase();
        let idx = upper.find(tag)?;
        let rest = &pnp[idx + tag.len()..];
        let val = rest
            .split(&['&', '\\'][..])
            .next()
            .unwrap_or("")
            .replace('_', " ")
            .trim()
            .to_string();
        if val.is_empty() {
            None
        } else {
            Some(val)
        }
    };
    (extract("VEN_"), extract("PROD_"), extract("REV_"))
}

fn query_windows_disk_metadata() -> HashMap<String, WinDiskMeta> {
    let mut map = HashMap::new();
    let ps_script = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
$ErrorActionPreference = 'SilentlyContinue'
$out = @()
Get-Partition | Where-Object { $_.DriveLetter } | ForEach-Object {
    $part = $_
    $disk = Get-Disk -Number $part.DiskNumber
    $phys = Get-PhysicalDisk | Where-Object { $_.DeviceId -eq [string]$part.DiskNumber } | Select-Object -First 1
    $wmi = Get-CimInstance Win32_DiskDrive | Where-Object { $_.Index -eq $part.DiskNumber } | Select-Object -First 1
    $pnp = if ($wmi) { [string]$wmi.PNPDeviceID } else { "" }
    $usbPnp = ""
    $usbHw = ""
    $usbVendor = ""
    $usbPort = 0
    $usbHub = ""
    if ($pnp) {
        $cur = $pnp
        for ($depth = 0; $depth -lt 4; $depth++) {
            if ($cur -match 'VID_[0-9A-Fa-f]{4}&PID_[0-9A-Fa-f]{4}') {
                $usbPnp = $cur
                $hwProp = Get-PnpDeviceProperty -InstanceId $cur -KeyName 'DEVPKEY_Device_HardwareIds'
                if ($hwProp -and $hwProp.Data) { $usbHw = ($hwProp.Data -join ';') }
                $mfgProp = Get-PnpDeviceProperty -InstanceId $cur -KeyName 'DEVPKEY_Device_Manufacturer'
                if ($mfgProp -and $mfgProp.Data) { $usbVendor = [string]$mfgProp.Data }
                $addrProp = Get-PnpDeviceProperty -InstanceId $cur -KeyName 'DEVPKEY_Device_Address'
                if ($addrProp -and $addrProp.Data) { $usbPort = [uint32]$addrProp.Data }
                $hubProp = Get-PnpDeviceProperty -InstanceId $cur -KeyName 'DEVPKEY_Device_Parent'
                if ($hubProp -and $hubProp.Data) { $usbHub = [string]$hubProp.Data }
                break
            }
            $parentProp = Get-PnpDeviceProperty -InstanceId $cur -KeyName 'DEVPKEY_Device_Parent'
            if (-not $parentProp -or -not $parentProp.Data) { break }
            $cur = [string]$parentProp.Data
        }
    }
    $out += [PSCustomObject]@{
        DriveLetter     = [string]$part.DriveLetter
        DiskNumber      = [uint32]$part.DiskNumber
        Model           = $disk.FriendlyName
        BusType         = [string]$disk.BusType
        SerialNumber    = $disk.SerialNumber
        Size            = [uint64]$disk.Size
        MediaType       = if ($phys) { [string]$phys.MediaType } else { "" }
        PnpId           = $pnp
        UsbPnpId        = $usbPnp
        UsbHwId         = $usbHw
        UsbVendor       = $usbVendor
        FirmwareVersion = if ($wmi) { [string]$wmi.FirmwareRevision } else { [string]$disk.FirmwareVersion }
        UsbPortNumber   = $usbPort
        UsbHubId        = $usbHub
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
    let trimmed = stdout.trim().trim_start_matches('\u{feff}');
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

struct UsbHubDescriptorInfo {
    usb_version: Option<String>,
    speed_label: Option<String>,
    max_power_ma: Option<u32>,
    bcd_device: Option<String>,
    manufacturer: Option<String>,
    product: Option<String>,
}

/// Reads exact USB 3.0/2.0 protocol version, negotiated link speed, bus power draw (mA),
/// bcdDevice, and USB iManufacturer / iProduct strings directly from the parent Windows USB Hub
/// via `IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX` (0x00220448) and
/// `IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION` (0x00220410) — no admin rights needed!
fn query_usb_hub_descriptors(hub_instance_id: &str, port_num: u32) -> Option<UsbHubDescriptorInfo> {
    const IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX: u32 = 0x00220448;
    const IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION: u32 = 0x00220410;

    let hub_path = format!(
        r"\\?\{}#{{f18a0e88-c30c-11d0-8815-00a0c906bed8}}",
        hub_instance_id.replace('\\', "#")
    );
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&hub_path)
        .ok()?;
    let handle = file.as_raw_handle();

    // Packed USB_NODE_CONNECTION_INFORMATION_EX buffer (1024 bytes fits up to 80+ pipes)
    let mut conn_buf = [0u8; 1024];
    conn_buf[0..4].copy_from_slice(&port_num.to_le_bytes());
    let mut bytes_ret: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX,
            conn_buf.as_mut_ptr() as *mut std::ffi::c_void,
            conn_buf.len() as u32,
            conn_buf.as_mut_ptr() as *mut std::ffi::c_void,
            conn_buf.len() as u32,
            &mut bytes_ret,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || bytes_ret < 35 {
        return None;
    }

    // Offset 4..22 = USB_DEVICE_DESCRIPTOR (18 bytes), Offset 23 = Speed (u8)
    let dd = &conn_buf[4..22];
    let bcd_usb = u16::from_le_bytes([dd[2], dd[3]]);
    let bcd_dev = u16::from_le_bytes([dd[12], dd[13]]);
    let i_mfg = dd[14];
    let i_prod = dd[15];
    let speed = conn_buf[23];

    let usb_version = if bcd_usb > 0 {
        let major = (bcd_usb >> 8) & 0xFF;
        let minor = bcd_usb & 0xFF;
        Some(format!("USB {:X}.{:02X}", major, minor))
    } else {
        None
    };

    let speed_label = match speed {
        0 => Some("低速 (LowSpeed 1.5Mbps)".to_string()),
        1 => Some("全速 (FullSpeed 12Mbps)".to_string()),
        2 => Some("高速 (HighSpeed 480Mbps)".to_string()),
        3 => Some("超高速 (SuperSpeed 5Gbps+)".to_string()),
        _ => None,
    };

    let bcd_device = Some(format!("{:04X}", bcd_dev));

    // Helper: fetch USB descriptor via IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION (packed 12B header + Data)
    let get_usb_desc = |desc_type: u8, desc_idx: u8, lang_id: u16| -> Option<Vec<u8>> {
        let mut req = [0u8; 12 + 256];
        req[0..4].copy_from_slice(&port_num.to_le_bytes());
        req[4] = 0x80; // bmRequest
        req[5] = 0x06; // bRequest = GET_DESCRIPTOR
        let w_value = ((desc_type as u16) << 8) | (desc_idx as u16);
        req[6..8].copy_from_slice(&w_value.to_le_bytes());
        req[8..10].copy_from_slice(&lang_id.to_le_bytes());
        req[10..12].copy_from_slice(&(256u16).to_le_bytes());
        let mut ret: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION,
                req.as_mut_ptr() as *mut std::ffi::c_void,
                req.len() as u32,
                req.as_mut_ptr() as *mut std::ffi::c_void,
                req.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 && ret > 14 {
            let data_len = (ret as usize - 12).min(256);
            Some(req[12..12 + data_len].to_vec())
        } else {
            None
        }
    };

    let max_power_ma = get_usb_desc(2, 0, 0).and_then(|cfg| {
        if cfg.len() >= 9 && cfg[1] == 2 {
            let raw = cfg[8] as u32;
            let mult = if bcd_usb >= 0x0300 { 8 } else { 2 };
            Some(raw * mult)
        } else {
            None
        }
    });

    let read_usb_string = |idx: u8| -> Option<String> {
        if idx == 0 {
            return None;
        }
        let raw = get_usb_desc(3, idx, 0x0409)?;
        if raw.len() < 4 || raw[1] != 3 {
            return None;
        }
        let b_len = (raw[0] as usize).min(raw.len());
        let u16s: Vec<u16> = raw[2..b_len]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&u16s).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };

    let manufacturer = read_usb_string(i_mfg);
    let product = read_usb_string(i_prod);

    Some(UsbHubDescriptorInfo {
        usb_version,
        speed_label,
        max_power_ma,
        bcd_device,
        manufacturer,
        product,
    })
}
