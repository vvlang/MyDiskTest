use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Embedded ChipGenius `Chips.wdb` database (compiled directly into the binary).
/// An external `Chips.wdb` placed beside the executable takes precedence if present!
const EMBEDDED_WDB: &[u8] = include_bytes!("../../Chips.wdb");
const XOR_KEY: [u8; 4] = [0x10, 0x18, 0x19, 0x86];

#[derive(Debug, Clone)]
pub struct ControllerRecord {
    pub vid: String,
    pub pid: String,
    pub chip_vendor: String,
    pub controller: String,
    pub device_vendor: String,
    pub note: String,
    pub url: String,
    pub rev_filter: String,
    pub from_supplement: bool,
}

#[derive(Debug, Clone)]
pub struct FlashRecord {
    /// 8-hex-char Flash ID (e.g. "983A98B3")
    pub flash_id: String,
    /// Decoded NAND manufacturer from first byte (e.g. "Toshiba / Kioxia (东芝/铠侠)")
    pub manufacturer: String,
    /// NAND cell type & page size (e.g. "TLC-16K", "MLC-8K", "SLC-2K", "QLC-16K")
    pub flash_type: String,
    /// Part numbers split from `#`-delimited list (e.g. ["[1CE]TC58TFG7T23TA0D"])
    pub part_numbers: Vec<String>,
    /// True if manufacturer is SpecTek (B5) or known downgraded/white-label NAND
    pub is_downgraded: bool,
}

#[derive(Debug, Clone)]
pub struct ControllerMatch {
    pub primary: ControllerRecord,
    pub candidates: Vec<ControllerRecord>,
    pub is_wildcard: bool,
}

#[derive(Debug, Clone)]
pub struct ChipDatabase {
    pub db_date: String,
    pub source_label: String,
    pub controllers: Vec<ControllerRecord>,
    pub flashes: Vec<FlashRecord>,
    by_vid_pid: HashMap<(String, String), Vec<usize>>,
    by_vid_wild: HashMap<String, Vec<usize>>,
    by_flash_id: HashMap<String, usize>,
}

impl ChipDatabase {
    /// Loads external `Chips.wdb` next to executable / CWD if found, otherwise uses built-in database.
    pub fn load_default() -> Self {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("Chips.wdb"));
                // Also support macOS .app/Contents/Resources/Chips.wdb
                if let Some(contents) = dir.parent() {
                    candidates.push(contents.join("Resources").join("Chips.wdb"));
                }
            }
        }
        candidates.push(PathBuf::from("Chips.wdb"));

        for path in candidates {
            if path.is_file() {
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(db) =
                        Self::from_wdb_bytes(&bytes, format!("外部数据库 ({})", path.display()))
                    {
                        return db;
                    }
                }
            }
        }

        Self::from_wdb_bytes(EMBEDDED_WDB, "内置 Chips.wdb 离线芯片库".to_string())
            .expect("embedded Chips.wdb must decode cleanly")
    }

    /// Loads a user-picked `Chips.wdb` file from disk.
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("读取 Chips.wdb 失败: {}", e))?;
        Self::from_wdb_bytes(&bytes, format!("外部数据库 ({})", path.display()))
    }

    pub fn from_wdb_bytes(data: &[u8], source_label: String) -> Result<Self, String> {
        // Header: optional 0xFF 0xFE marker + ASCII date line ending with \r\n or \n
        let mut pos = 0;
        if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xFE {
            pos = 2;
        }
        let nl = data[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .ok_or_else(|| "Chips.wdb 文件头格式无效".to_string())?;
        let header_end = pos + nl;
        let date_raw = &data[pos..header_end];
        let db_date = String::from_utf8_lossy(date_raw)
            .trim_end_matches('\r')
            .trim()
            .to_string();

        let body = &data[header_end + 1..];
        let mut decrypted = Vec::with_capacity(body.len());
        for (i, &b) in body.iter().enumerate() {
            decrypted.push(b ^ XOR_KEY[i % 4]);
        }

        let (decoded, _, _) = encoding_rs::GB18030.decode(&decrypted);
        let mut controllers = Vec::with_capacity(650);
        let mut flashes = Vec::with_capacity(1200);
        let mut by_vid_pid: HashMap<(String, String), Vec<usize>> = HashMap::new();
        let mut by_vid_wild: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_flash_id: HashMap<String, usize> = HashMap::new();

        for (line_idx, raw_line) in decoded.lines().enumerate() {
            let mut line = raw_line.trim_matches('\0').trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            if line_idx == 0 && line.starts_with("GJ") {
                line = &line[2..];
            }

            if line.contains('\t') {
                // Controller record (8 tab-separated columns)
                let cols: Vec<&str> = line.split('\t').collect();
                if cols.len() >= 4 {
                    let vid = normalize_hex4(cols[0]);
                    let pid_raw = cols[1].trim();
                    let pid = if pid_raw == "*" {
                        "*".to_string()
                    } else {
                        normalize_hex4(pid_raw)
                    };
                    if vid.len() == 4 {
                        let rec = ControllerRecord {
                            vid: vid.clone(),
                            pid: pid.clone(),
                            chip_vendor: cols.get(2).copied().unwrap_or("").trim().to_string(),
                            controller: cols.get(3).copied().unwrap_or("").trim().to_string(),
                            device_vendor: cols.get(4).copied().unwrap_or("").trim().to_string(),
                            note: cols.get(5).copied().unwrap_or("").trim().to_string(),
                            url: cols.get(6).copied().unwrap_or("").trim().to_string(),
                            rev_filter: cols.get(7).copied().unwrap_or("").trim().to_string(),
                            from_supplement: false,
                        };
                        let idx = controllers.len();
                        controllers.push(rec);
                        if pid == "*" {
                            by_vid_wild.entry(vid).or_default().push(idx);
                        } else if pid.len() == 4 {
                            by_vid_pid.entry((vid, pid)).or_default().push(idx);
                        }
                    }
                }
            } else if line.len() >= 16 && line[..8].bytes().all(|b| b.is_ascii_hexdigit()) {
                // NAND Flash ID record: [0..8] Flash ID, [8..16] Type-PageSize, [16..] Part numbers
                let fid = line[..8].to_ascii_uppercase();
                let ftype = line[8..16].trim().to_string();
                let parts_raw = line[16..].trim();
                let part_numbers: Vec<String> = parts_raw
                    .split('#')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let mfg_byte = u8::from_str_radix(&fid[..2], 16).unwrap_or(0);
                let manufacturer = nand_manufacturer_name(mfg_byte).to_string();
                let is_downgraded = mfg_byte == 0xB5;

                let idx = flashes.len();
                flashes.push(FlashRecord {
                    flash_id: fid.clone(),
                    manufacturer,
                    flash_type: ftype,
                    part_numbers,
                    is_downgraded,
                });
                by_flash_id.entry(fid).or_insert(idx);
            }
        }

        // Append supplemental modern 2020-2026 USB 3.2 / USB4 / NVMe bridge controllers
        // ONLY when not already present in Chips.wdb so Chips.wdb remains authoritative.
        for rec in supplemental_controllers() {
            let key = (rec.vid.clone(), rec.pid.clone());
            if !by_vid_pid.contains_key(&key) {
                let idx = controllers.len();
                controllers.push(rec);
                by_vid_pid.entry(key).or_default().push(idx);
            }
        }

        if controllers.is_empty() && flashes.is_empty() {
            return Err("未在 Chips.wdb 中解析到有效芯片记录".to_string());
        }

        Ok(Self {
            db_date,
            source_label,
            controllers,
            flashes,
            by_vid_pid,
            by_vid_wild,
            by_flash_id,
        })
    }

    /// Looks up USB controller info by VID, PID, and optional bcdDevice / firmware revision.
    pub fn lookup_controller(
        &self,
        vid_raw: &str,
        pid_raw: &str,
        rev: Option<&str>,
    ) -> Option<ControllerMatch> {
        let vid = normalize_hex4(vid_raw);
        let pid = normalize_hex4(pid_raw);
        if vid.len() != 4 {
            return None;
        }

        if let Some(indices) = self.by_vid_pid.get(&(vid.clone(), pid.clone())) {
            let mut recs: Vec<ControllerRecord> =
                indices.iter().map(|&i| self.controllers[i].clone()).collect();
            // Sort best match first: revision match > non-empty controller > Chips.wdb entry
            if let Some(rev_str) = rev {
                let rev_clean = rev_str.trim().to_ascii_lowercase();
                recs.sort_by_key(|r| {
                    let rev_hit = !rev_clean.is_empty()
                        && !r.rev_filter.is_empty()
                        && (r.rev_filter.to_ascii_lowercase().contains(&rev_clean)
                            || rev_clean.contains(&r.rev_filter.to_ascii_lowercase()));
                    let has_ctrl = !r.controller.is_empty();
                    (!rev_hit, !has_ctrl, r.from_supplement)
                });
            }
            if let Some(primary) = recs.first().cloned() {
                return Some(ControllerMatch {
                    primary,
                    candidates: recs,
                    is_wildcard: false,
                });
            }
        }

        // Wildcard VID match (identifies at least the controller vendor!)
        if let Some(indices) = self.by_vid_wild.get(&vid) {
            let recs: Vec<ControllerRecord> =
                indices.iter().map(|&i| self.controllers[i].clone()).collect();
            if let Some(primary) = recs.first().cloned() {
                return Some(ControllerMatch {
                    primary,
                    candidates: recs,
                    is_wildcard: true,
                });
            }
        }

        None
    }

    /// Looks up a NAND Flash ID (e.g. "98 3A 98 B3 76 72" or "983A98B3").
    pub fn lookup_flash_id(&self, raw_hex: &str) -> Option<FlashRecord> {
        let clean: String = raw_hex
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .collect::<String>()
            .to_ascii_uppercase();
        if clean.len() < 8 {
            return None;
        }
        let key8 = &clean[..8];
        if let Some(&idx) = self.by_flash_id.get(key8) {
            return Some(self.flashes[idx].clone());
        }
        // Fallback: 6-hex prefix match (same manufacturer + device + 3rd ID byte)
        let key6 = &clean[..6];
        self.flashes
            .iter()
            .find(|f| f.flash_id.starts_with(key6))
            .cloned()
    }

    /// Interactive search across controllers by VID, PID, vendor, controller model, or note.
    pub fn search_controllers(&self, query: &str, limit: usize) -> Vec<&ControllerRecord> {
        let q = query.trim().to_ascii_lowercase();
        let tokens: Vec<&str> = q.split_whitespace().collect();
        self.controllers
            .iter()
            .filter(|r| {
                if tokens.is_empty() {
                    return true;
                }
                let hay = format!(
                    "{} {} {} {} {} {} {}",
                    r.vid,
                    r.pid,
                    r.chip_vendor,
                    r.controller,
                    r.device_vendor,
                    r.note,
                    r.rev_filter
                )
                .to_ascii_lowercase();
                tokens.iter().all(|t| hay.contains(t))
            })
            .take(limit)
            .collect()
    }

    /// Interactive search across NAND Flash records by Flash ID, manufacturer, type, or part number.
    pub fn search_flashes(&self, query: &str, limit: usize) -> Vec<&FlashRecord> {
        let q = query.trim().to_ascii_lowercase();
        let q_no_space: String = q.chars().filter(|c| !c.is_whitespace() && *c != ':').collect();
        let tokens: Vec<&str> = q.split_whitespace().collect();
        self.flashes
            .iter()
            .filter(|f| {
                if tokens.is_empty() {
                    return true;
                }
                if q_no_space.len() >= 4
                    && q_no_space.chars().all(|c| c.is_ascii_hexdigit())
                    && f.flash_id.to_ascii_lowercase().starts_with(&q_no_space[..q_no_space.len().min(8)])
                {
                    return true;
                }
                let hay = format!(
                    "{} {} {} {}",
                    f.flash_id,
                    f.manufacturer,
                    f.flash_type,
                    f.part_numbers.join(" ")
                )
                .to_ascii_lowercase();
                tokens.iter().all(|t| hay.contains(t))
            })
            .take(limit)
            .collect()
    }
}

pub fn normalize_hex4(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let hex: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_uppercase();
    if hex.is_empty() {
        String::new()
    } else if hex.len() < 4 {
        format!("{:0>4}", hex)
    } else {
        hex[..4].to_string()
    }
}

/// Maps NAND Flash 1st ID byte (JEDEC Manufacturer ID) to human-readable vendor name.
pub fn nand_manufacturer_name(id: u8) -> &'static str {
    match id {
        0x45 => "SanDisk (闪迪)",
        0x2C => "Micron (美光)",
        0xAD => "SK Hynix (海力士)",
        0x98 => "Toshiba / Kioxia (东芝 / 铠侠)",
        0xEC => "Samsung (三星)",
        0x89 => "Intel (英特尔)",
        0x20 => "ST / Numonyx (意法半导体)",
        0x01 => "Spansion / Cypress (飞索)",
        0xB5 => "SpecTek (美光大S降级片)",
        0x9B => "YMTC (长江存储)",
        0xC8 => "GigaDevice (兆易创新)",
        0xC2 => "Macronix (旺宏)",
        0xEF => "Winbond (华邦)",
        0xF8 => "Fidelix / Dosilicon (东芯)",
        0x92 => "ESMT (晶豪)",
        0x99 => "MIRA / Powerchip",
        0xCD => "Powerchip (力晶)",
        _ => "Unknown (未知闪存厂商)",
    }
}

/// Returns true if byte 0 is a recognized JEDEC NAND Flash manufacturer ID.
pub fn is_known_nand_vendor(id: u8) -> bool {
    matches!(
        id,
        0x45 | 0x2C
            | 0xAD
            | 0x98
            | 0xEC
            | 0x89
            | 0x20
            | 0x01
            | 0xB5
            | 0x9B
            | 0xC8
            | 0xC2
            | 0xEF
            | 0xF8
            | 0x92
    )
}

fn supplemental_controllers() -> Vec<ControllerRecord> {
    let entries: &[(&str, &str, &str, &str, &str)] = &[
        ("0BDA", "9210", "Realtek (瑞昱)", "RTL9210 / RTL9210B (USB 3.2 NVMe/SATA 桥接)", "常见于 10Gbps 固态硬盘盒与高速固态 U 盘"),
        ("0BDA", "9220", "Realtek (瑞昱)", "RTL9220DP (USB 3.2 Gen2x2 20Gbps 桥接)", "20Gbps 高速 NVMe 桥接主控"),
        ("152D", "0583", "JMicron (智微)", "JMS583 (USB 3.1 Gen2 NVMe 桥接)", "常见于 10Gbps NVMe 移动固态硬盘盒"),
        ("152D", "0578", "JMicron (智微)", "JMS578 (USB 3.0 SATA 桥接)", "常见于 SATA 移动硬盘盒"),
        ("174C", "2362", "ASMedia (祥硕)", "ASM2362 (USB 3.1 Gen2 NVMe 桥接)", "10Gbps NVMe 桥接主控"),
        ("174C", "2364", "ASMedia (祥硕)", "ASM2364 (USB 3.2 Gen2x2 20Gbps 桥接)", "20Gbps NVMe 桥接主控"),
        ("174C", "2464", "ASMedia (祥硕)", "ASM2464PD (USB4 / Thunderbolt 4 40Gbps)", "40Gbps USB4 旗舰硬盘盒主控"),
        ("090C", "2000", "Silicon Motion (慧荣)", "SM2320 / SM3282 (USB 3.2 固态 U 盘主控)", "单芯片高速 USB 3.2 固态 U 盘主控"),
        ("090C", "3281", "Silicon Motion (慧荣)", "SM3281L / SM3281AB", "USB 3.2 Gen1 主流闪存主控"),
        ("13FE", "6500", "Phison (群联)", "PS2251-17 / PS2251-19 (USB 3.2)", "USB 3.2 高速 U 盘主控"),
        ("13FE", "6700", "Phison (群联)", "PS2251-19 / U17 (USB 3.2)", "USB 3.2 主流 U 盘主控"),
        ("0781", "5583", "SanDisk (闪迪)", "SanDisk Ultra Fit / CZ430 原厂主控", "闪迪原厂一体化封装"),
        ("0781", "5581", "SanDisk (闪迪)", "SanDisk Ultra / CZ48 原厂主控", "闪迪原厂主控"),
        ("0781", "5588", "SanDisk (闪迪)", "SanDisk Extreme Pro CZ880 (固态主控)", "高速固态级 U 盘"),
        ("0781", "5591", "SanDisk (闪迪)", "SanDisk Ultra Luxe / Dual Drive", "闪迪原厂主控"),
        ("0781", "55A3", "SanDisk (闪迪)", "SanDisk Extreme Pro USB 3.2", "高速固态级 U 盘"),
        ("090C", "1000", "Silicon Motion (慧荣)", "SM3267 / SM3268 / SM3281 系列", "慧荣通用 VID/PID，具体型号取决于固件版本"),
        ("0951", "1666", "Kingston (金士顿)", "DataTraveler 100 G3 / Exodia (群联/慧荣代工)", "金士顿零售盘（常见 PS2251 或 SM3281 方案）"),
        ("0951", "177F", "Kingston (金士顿)", "DataTraveler Max (SMI SM2320)", "10Gbps 固态级 U 盘"),
        ("04E8", "61F5", "Samsung (三星)", "Samsung PSSD T5 / T7 (三星原厂主控)", "三星原厂移动固态硬盘"),
        ("04E8", "4001", "Samsung (三星)", "Samsung PSSD T7 Shield / T9", "三星原厂高速移动固态硬盘"),
        ("090C", "3000", "Silicon Motion (慧荣)", "SM32xx Recovery / 量产模式", "设备处于慧荣工程恢复模式"),
    ];

    entries
        .iter()
        .map(|(vid, pid, vendor, ctrl, note)| ControllerRecord {
            vid: (*vid).to_string(),
            pid: (*pid).to_string(),
            chip_vendor: (*vendor).to_string(),
            controller: (*ctrl).to_string(),
            device_vendor: String::new(),
            note: (*note).to_string(),
            url: String::new(),
            rev_filter: String::new(),
            from_supplement: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_chips_wdb_parses_all_controllers_and_flashes() {
        let db = ChipDatabase::load_default();
        assert_eq!(db.db_date, "2019-12-29");
        assert!(db.controllers.len() >= 596);
        assert_eq!(db.flashes.len(), 1136);

        // Controller exact lookup
        let m = db
            .lookup_controller("0x0204", "0x6025", Some("2.00"))
            .expect("Chipsbank CBM2080");
        assert!(m.primary.chip_vendor.contains("Chipsbank"));
        assert!(m.primary.controller.contains("CBM"));
        assert!(!m.is_wildcard);

        // Controller wildcard lookup
        let w = db
            .lookup_controller("0204", "FFFF", None)
            .expect("Chipsbank wildcard");
        assert!(w.is_wildcard);
        assert!(w.primary.chip_vendor.contains("Chipsbank"));

        // Flash ID lookup with spaces
        let f = db
            .lookup_flash_id("98 3A 98 B3 76 72")
            .expect("Toshiba TLC flash");
        assert_eq!(f.flash_id, "983A98B3");
        assert!(f.manufacturer.contains("Toshiba"));
        assert_eq!(f.flash_type, "TLC-16K");
        assert!(f.part_numbers.iter().any(|p| p.contains("TC58TFG7T23TA0D")));

        // SpecTek downgraded flash warning flag
        let spectek = db
            .flashes
            .iter()
            .find(|f| f.flash_id.starts_with("B5"))
            .expect("SpecTek entry");
        assert!(spectek.is_downgraded);
    }
}
