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

        // Append supplemental modern 2020-2026 NAND Flash IDs (Kioxia BiCS4/5/6, Micron B27/B47R/N38A,
        // YMTC X2/X3, Samsung V6/V7, SK Hynix V6/V7, SpecTek) when not already present in Chips.wdb.
        for rec in supplemental_flashes() {
            if !by_flash_id.contains_key(&rec.flash_id) {
                let idx = flashes.len();
                let fid = rec.flash_id.clone();
                flashes.push(rec);
                by_flash_id.insert(fid, idx);
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

    /// Looks up USB controller info by VID, PID, optional bcdDevice, and optional SCSI Inquiry strings.
    pub fn lookup_controller(
        &self,
        vid_raw: &str,
        pid_raw: &str,
        rev: Option<&str>,
    ) -> Option<ControllerMatch> {
        self.lookup_controller_with_scsi(vid_raw, pid_raw, rev, None, None, None)
    }

    pub fn lookup_controller_with_scsi(
        &self,
        vid_raw: &str,
        pid_raw: &str,
        rev: Option<&str>,
        scsi_vendor: Option<&str>,
        scsi_product: Option<&str>,
        scsi_rev: Option<&str>,
    ) -> Option<ControllerMatch> {
        let vid = normalize_hex4(vid_raw);
        let pid = normalize_hex4(pid_raw);
        if vid.len() != 4 {
            return infer_controller_from_scsi(scsi_vendor, scsi_product, scsi_rev);
        }

        let effective_rev = scsi_rev.or(rev);

        if let Some(indices) = self.by_vid_pid.get(&(vid.clone(), pid.clone())) {
            let mut recs: Vec<ControllerRecord> =
                indices.iter().map(|&i| self.controllers[i].clone()).collect();
            // Sort best match first: revision match > non-empty controller > Chips.wdb entry
            if let Some(rev_str) = effective_rev {
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
            if let Some(mut primary) = recs.first().cloned() {
                // Enrich wildcard match with SCSI Inquiry heuristic if controller model is empty
                if primary.controller.is_empty() {
                    if let Some(heur) = infer_controller_from_scsi(scsi_vendor, scsi_product, scsi_rev) {
                        primary.controller = heur.primary.controller;
                        if primary.note.is_empty() {
                            primary.note = heur.primary.note;
                        }
                    }
                }
                return Some(ControllerMatch {
                    primary,
                    candidates: recs,
                    is_wildcard: true,
                });
            }
        }

        // Fallback: SCSI Inquiry firmware signature heuristics (e.g. PMAP, 1100, 8.07, General UDisk 5.00)
        infer_controller_from_scsi(scsi_vendor, scsi_product, scsi_rev)
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

    /// Exact 4-byte binary Flash ID lookup (used to scan controller SCSI response payloads
    /// with near-zero false positive rate: 1136 / 2^32 ≈ 0.000026%).
    #[allow(dead_code)]
    pub fn lookup_exact_4bytes(&self, bytes: &[u8]) -> Option<FlashRecord> {
        if bytes.len() < 4 {
            return None;
        }
        let key = format!(
            "{:02X}{:02X}{:02X}{:02X}",
            bytes[0], bytes[1], bytes[2], bytes[3]
        );
        self.by_flash_id.get(&key).map(|&idx| self.flashes[idx].clone())
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
        0xF8 | 0xE5 => "Fidelix / DoSilicon (东芯)",
        0x92 => "ESMT (晶豪)",
        0x99 => "MIRA / Powerchip",
        0xCD => "PSMC / Powerchip (力晶)",
        0x07 => "Renesas (瑞萨)",
        0xC1 => "Infineon (英飞凌)",
        _ => "Unknown (未知闪存厂商)",
    }
}

/// Returns true if byte 0 is a recognized JEDEC NAND Flash manufacturer ID.
#[allow(dead_code)]
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
            | 0xE5
            | 0x92
            | 0xCD
            | 0x07
            | 0xC1
    )
}

/// Algorithmic fallback decoder for brand-new 2020-2026 6-byte JEDEC/ONFI NAND Flash IDs
/// not yet present in `Chips.wdb` or `supplemental_flashes()`.
pub fn decode_flash_id_algorithmic(fid: &[u8; 6]) -> Option<FlashRecord> {
    if !is_known_nand_vendor(fid[0]) {
        return None;
    }
    let mfg = nand_manufacturer_name(fid[0]).to_string();
    let is_downgraded = fid[0] == 0xB5;
    // Standard JEDEC 6-byte NAND ID:
    //   Byte 2 bits [3:2]: cell level (00=SLC, 01=MLC, 10=TLC, 11=QLC)
    //   Byte 3 bits [1:0]: page size (modern 3D NAND: 00=4K, 01=8K, 10=16K, 11=32K/16K)
    let cell_bits = (fid[2] >> 2) & 0x03;
    let cell = match cell_bits {
        0 => "SLC",
        1 => "MLC",
        2 => "TLC",
        _ => "QLC",
    };
    let page_kb = match fid[3] & 0x03 {
        0 => "4K",
        1 => "8K",
        2 => "16K",
        _ => "16K/32K",
    };
    let flash_id = format!("{:02X}{:02X}{:02X}{:02X}", fid[0], fid[1], fid[2], fid[3]);
    let full_hex = format!(
        "{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
        fid[0], fid[1], fid[2], fid[3], fid[4], fid[5]
    );
    Some(FlashRecord {
        flash_id,
        manufacturer: mfg,
        flash_type: format!("{}-{}", cell, page_kb),
        part_numbers: vec![format!("JEDEC 实时解析 ({}，库外新款颗粒)", full_hex)],
        is_downgraded,
    })
}

/// Infers controller family from classic USB Mass Storage SCSI Inquiry firmware signatures
/// (used by ChipGenius when VID/PID is OEM-rebranded or spoofed).
pub fn infer_controller_from_scsi(
    vendor: Option<&str>,
    product: Option<&str>,
    rev: Option<&str>,
) -> Option<ControllerMatch> {
    let v = vendor.unwrap_or("").trim().to_ascii_uppercase();
    let p = product.unwrap_or("").trim().to_ascii_uppercase();
    let r = rev.unwrap_or("").trim().to_ascii_uppercase();

    let (chip_vendor, controller, note) = if r == "PMAP" || p.contains("2307") || p.contains("2251") {
        (
            "Phison (群联)",
            "PS2251 系列 (依据 SCSI PMAP 固件特征识别)",
            "SCSI 修订号 PMAP 为群联 PS2251 主控典型特征",
        )
    } else if r == "1100" && (v.contains("SMI") || p.contains("USB DISK") || p.contains("FLASH") || v.contains("GENERIC") || v.contains("GENERAL")) {
        (
            "Silicon Motion (慧荣)",
            "SM3267 / SM3268 / SM3281 系列 (依据 SCSI 1100 特征识别)",
            "SCSI 修订号 1100 为慧荣 SM32xx 主控常见默认固件版本",
        )
    } else if (r == "8.07" || r == "8.00") && (p.contains("FLASH DISK") || v.contains("GENERIC") || v.contains("ALCOR")) {
        (
            "Alcor Micro (安国)",
            "AU6989SN / AU6989SN-GT 系列 (依据 SCSI 8.07 特征识别)",
            "Generic Flash Disk + Rev 8.07 为安国 AU6989 主控经典特征",
        )
    } else if (v.contains("GENERAL") && p.contains("UDISK")) || (r == "5.00" && p.contains("UDISK")) {
        (
            "Chipsbank (芯邦) / FirstChip (一芯)",
            "CBM2099E / CBM2199E / FC1179 (依据 General UDisk 5.00 特征)",
            "提醒：General UDisk 5.00 常见于芯邦/一芯公版盘及扩容黑盘，建议运行扩容测试核验！",
        )
    } else if v.contains("INNOSTOR") || p.contains("IS903") || p.contains("IS917") || p.contains("IS918") {
        (
            "Innostor (银灿)",
            "IS903 / IS917 / IS918 / IS918M 系列",
            "银灿高速 USB 3.0/3.1 主控方案",
        )
    } else if r == "6.60" || r == "6.70" || r == "6.80" {
        (
            "HY / FirstChip 系",
            "HY6919 系列 (依据 SCSI 6.60/6.70/6.80 特征)",
            "ChipGenius v4.21 专属 Rev 6.60~6.80 主控特征",
        )
    } else if v.contains("SKYMEDI") || r == "8.20" {
        (
            "Skymedi (擎泰)",
            "SK6211 / SK6221 / SK6231 系列",
            "擎泰 U 盘主控方案",
        )
    } else if v.contains("NETAC") || p.contains("ONLYDISK") {
        (
            "Netac / Appotech (朗科)",
            "Netac / Appotech / SMI 定制主控",
            "朗科 OnlyDisk 零售盘方案",
        )
    } else if v.contains("SANDISK") {
        (
            "SanDisk (闪迪原厂)",
            "SanDisk 原厂一体化主控",
            "闪迪原厂封装（不支持公开第三方量产指令）",
        )
    } else if v.contains("SAMSUNG") && (p.contains("FLASH") || p.contains("BAR") || p.contains("FIT") || p.contains("PSSD")) {
        (
            "Samsung (三星原厂)",
            "Samsung 原厂主控",
            "三星原厂一体化封装方案",
        )
    } else if v.contains("KIOXIA") || v.contains("TOSHIBA") {
        (
            "Kioxia / Toshiba (铠侠 / 东芝)",
            "Phison 定制 / 东芝原厂主控",
            "铠侠/东芝零售 U 盘常见为群联定制主控 + 东芝原厂闪存",
        )
    } else {
        return None;
    };

    let rec = ControllerRecord {
        vid: "----".to_string(),
        pid: "----".to_string(),
        chip_vendor: chip_vendor.to_string(),
        controller: controller.to_string(),
        device_vendor: vendor.unwrap_or("").to_string(),
        note: note.to_string(),
        url: String::new(),
        rev_filter: rev.unwrap_or("").to_string(),
        from_supplement: true,
    };
    Some(ControllerMatch {
        primary: rec.clone(),
        candidates: vec![rec],
        is_wildcard: true,
    })
}

fn supplemental_controllers() -> Vec<ControllerRecord> {
    let entries: &[(&str, &str, &str, &str, &str)] = &[
        // ---- Classic & Modern High-Volume Flash Controllers missing from 2019 Chips.wdb ----
        ("ABCD", "1234", "Chipsbank (芯邦) / FirstChip (一芯)", "CBM2099E / CBM2199E / FC1179", "提醒：ABCD:1234 为公版/山寨黑盘高频 VID/PID，强烈建议执行扩容测试！"),
        ("048D", "1234", "Chipsbank (芯邦) / FirstChip (一芯)", "CBM2099 / CBM2199 / FC1178 / FC1179", "提醒：048D:1234 常见于公版盘及扩容黑盘，建议核验真实容量！"),
        ("346D", "5678", "FirstChip (一芯)", "FC1178 / FC1179 / FC1308", "一芯公版 U 盘主控方案（常见于廉价/礼品盘，建议检测扩容）"),
        ("1E3D", "198A", "Chipsbank (芯邦)", "CBM2099 / CBM2099E / CBM2199", "芯邦经典 USB 2.0 主控"),
        ("1E3D", "2093", "Chipsbank (芯邦)", "CBM2093", "芯邦 USB 2.0 主控"),
        ("1E3D", "2095", "Chipsbank (芯邦)", "CBM2095 / CBM2096", "芯邦 USB 2.0 主控"),
        ("1E3D", "2096", "Chipsbank (芯邦)", "CBM2096P / CBM2098", "芯邦 USB 2.0 主控"),
        ("1E3D", "2098", "Chipsbank (芯邦)", "CBM2098E / CBM2098S", "芯邦 USB 2.0 主控"),
        ("1E3D", "2099", "Chipsbank (芯邦)", "CBM2099E / CBM2099S", "芯邦高性价比主流 U 盘主控"),
        ("1E3D", "2199", "Chipsbank (芯邦)", "CBM2199 / CBM2199E / CBM2199S", "芯邦 USB 2.0/3.0 主控"),
        // ---- Alcor Micro (安国) ----
        ("058F", "6387", "Alcor Micro (安国)", "AU6989SN / AU6989SN-GT / AU6989AN", "安国最高频公版 VID/PID（Windows 管理员下可直读具体型号与闪存）"),
        ("058F", "1234", "Alcor Micro (安国)", "AU6989 量产/无闪存模式", "安国主控空片或量产模式"),
        ("058F", "6366", "Alcor Micro (安国)", "AU6438 / AU6465 (SD/TF 读卡器主控)", "安国高速读卡器主控"),
        ("058F", "6364", "Alcor Micro (安国)", "AU6477 (多合一读卡器主控)", "安国读卡器主控"),
        // ---- Silicon Motion (慧荣 SMI) ----
        ("090C", "1000", "Silicon Motion (慧荣)", "SM3267 / SM3268 / SM3271 / SM3281 系列", "慧荣通用 VID/PID（Windows 管理员下可直读具体主控与闪存 Flash ID）"),
        ("090C", "2000", "Silicon Motion (慧荣)", "SM2320 / SM3282 (USB 3.2 固态 U 盘主控)", "单芯片高速 USB 3.2 固态 U 盘主控"),
        ("090C", "3000", "Silicon Motion (慧荣)", "SM32xx Recovery / 量产模式", "设备处于慧荣工程恢复/量产模式"),
        ("090C", "3267", "Silicon Motion (慧荣)", "SM3267AE / SM3267AB", "慧荣 USB 3.0 主流闪存主控"),
        ("090C", "3268", "Silicon Motion (慧荣)", "SM3268AB", "慧荣 USB 3.0 主控"),
        ("090C", "3281", "Silicon Motion (慧荣)", "SM3281L / SM3281AB", "慧荣 USB 3.2 Gen1 主流闪存主控"),
        // ---- Phison (群联) ----
        ("13FE", "4100", "Phison (群联)", "PS2251-67 (2267)", "群联 USB 2.0 经典主控"),
        ("13FE", "4200", "Phison (群联)", "PS2251-68 (2268)", "群联 USB 2.0 主控"),
        ("13FE", "4300", "Phison (群联)", "PS2251-68 / PS2251-70", "群联主流 U 盘主控"),
        ("13FE", "5100", "Phison (群联)", "PS2251-03 (2303)", "群联 USB 3.0 双通道高速主控"),
        ("13FE", "5200", "Phison (群联)", "PS2251-03 / PS2251-07", "群联 USB 3.0 高速主控"),
        ("13FE", "5500", "Phison (群联)", "PS2251-07 (2307)", "群联高人气 USB 3.0 主控"),
        ("13FE", "6300", "Phison (群联)", "PS2251-09 (2309) / PS2251-11", "群联 USB 3.1 高速主控"),
        ("13FE", "6500", "Phison (群联)", "PS2251-17 (2317) / PS2251-19 (USB 3.2)", "群联 USB 3.2 高速 U 盘主控"),
        ("13FE", "6700", "Phison (群联)", "PS2251-19 / U17 (USB 3.2)", "群联 USB 3.2 主流 U 盘主控"),
        // ---- Innostor (银灿) ----
        ("1F75", "0902", "Innostor (银灿)", "IS902 / IS902E (USB 3.0 双通道)", "银灿经典双通道高速主控"),
        ("1F75", "0903", "Innostor (银灿)", "IS903 (USB 3.0 双贴神级主控)", "银灿发烧级双通道 SLC/MLC 主控"),
        ("1F75", "0916", "Innostor (银灿)", "IS916 / IS916EN", "银灿 USB 3.0 主控"),
        ("1F75", "0917", "Innostor (银灿)", "IS917 (USB 3.0 单通道)", "银灿高兼容性 USB 3.0 主控"),
        ("1F75", "0918", "Innostor (银灿)", "IS918 / IS918M", "银灿 USB 3.1 主控"),
        // ---- Skymedi (擎泰) ----
        ("1516", "8628", "Skymedi (擎泰)", "SK6221 / SK6226", "擎泰 USB 2.0/3.0 主控"),
        ("1516", "1213", "Skymedi (擎泰)", "SK6211 / SK6215", "擎泰 USB 2.0 主控"),
        // ---- SanDisk (闪迪) ----
        ("0781", "5567", "SanDisk (闪迪)", "SanDisk Cruzer Blade (CZ50) 原厂主控", "闪迪原厂一体化封装"),
        ("0781", "5571", "SanDisk (闪迪)", "SanDisk Cruzer Fit (CZ33) 原厂主控", "闪迪原厂迷你盘"),
        ("0781", "5575", "SanDisk (闪迪)", "SanDisk Cruzer Glide (CZ60) 原厂主控", "闪迪原厂主控"),
        ("0781", "5580", "SanDisk (闪迪)", "SanDisk Extreme (CZ80) 高速固态主控", "闪迪经典高速固态 U 盘"),
        ("0781", "5581", "SanDisk (闪迪)", "SanDisk Ultra (CZ48) 原厂主控", "闪迪原厂 USB 3.0 主控"),
        ("0781", "5583", "SanDisk (闪迪)", "SanDisk Ultra Fit (CZ43/CZ430) 原厂主控", "闪迪原厂一体化封装"),
        ("0781", "5588", "SanDisk (闪迪)", "SanDisk Extreme Pro (CZ880) 固态主控", "高速固态级 U 盘（支持 SAT/SMART）"),
        ("0781", "5590", "SanDisk (闪迪)", "SanDisk Ultra Dual Drive (DD2/DD3)", "闪迪双接口原厂主控"),
        ("0781", "5591", "SanDisk (闪迪)", "SanDisk Ultra Flair (CZ73) / Ultra Luxe", "闪迪金属原厂主控"),
        ("0781", "5595", "SanDisk (闪迪)", "SanDisk Extreme Go (CZ800/CZ810)", "闪迪高速原厂主控"),
        ("0781", "5597", "SanDisk (闪迪)", "SanDisk Ultra Shift (CZ410)", "闪迪原厂主控"),
        ("0781", "5599", "SanDisk (闪迪)", "SanDisk Ultra Dual Drive Go (DDC3)", "闪迪 Type-C 双接口原厂主控"),
        ("0781", "55A3", "SanDisk (闪迪)", "SanDisk Extreme Pro USB 3.2 (CZ880 新版)", "高速固态级 U 盘"),
        ("0781", "55AE", "SanDisk (闪迪)", "SanDisk Extreme Portable SSD (E61/E81)", "闪迪移动固态硬盘"),
        // ---- Kingston (金士顿) ----
        ("0951", "1665", "Kingston (金士顿)", "DataTraveler 100 G3 (Phison/SMI 方案)", "金士顿零售盘（Windows 管理员下可直读群联/慧荣精确主控）"),
        ("0951", "1666", "Kingston (金士顿)", "DataTraveler 100 G3 / Exodia (PS2251/SM3281)", "金士顿高频零售盘（常见群联或慧荣方案）"),
        ("0951", "16A2", "Kingston (金士顿)", "DataTraveler SE9 G2", "金士顿金属盘"),
        ("0951", "16BE", "Kingston (金士顿)", "DataTraveler Micro 3.1", "金士顿迷你 USB 3.1 盘"),
        ("0951", "16D5", "Kingston (金士顿)", "DataTraveler Kyson / Exodia", "金士顿 USB 3.2 零售盘"),
        ("0951", "172B", "Kingston (金士顿)", "DataTraveler Exodia M / DTX", "金士顿 USB 3.2 零售盘"),
        ("0951", "177F", "Kingston (金士顿)", "DataTraveler Max (SMI SM2320 固态主控)", "10Gbps 固态级旗舰 U 盘"),
        // ---- Toshiba / Kioxia / Transcend / Samsung / Lexar ----
        ("0930", "6544", "Toshiba / Kioxia (东芝/铠侠)", "Phison 定制主控 (TC58/PS2251)", "东芝/铠侠原厂 U 盘"),
        ("0930", "6545", "Toshiba / Kioxia (东芝/铠侠)", "Phison 定制主控 (隼闪/随闪系列)", "东芝/铠侠原厂 U 盘"),
        ("8564", "1000", "Transcend (创见)", "Silicon Motion (慧荣) 定制主控", "创见 JetFlash 系列（多为慧荣 SM3267/3281 方案）"),
        ("04E8", "61F5", "Samsung (三星)", "Samsung PSSD T5 / T7 (三星原厂主控)", "三星原厂移动固态硬盘"),
        ("04E8", "61F6", "Samsung (三星)", "Samsung PSSD T7 Touch / Shield", "三星原厂移动固态硬盘"),
        ("04E8", "4001", "Samsung (三星)", "Samsung PSSD T7 Shield / T9", "三星原厂高速移动固态硬盘"),
        ("21C4", "0CD1", "Lexar (雷克沙)", "Lexar JumpDrive 系列", "雷克沙零售盘"),
        // ---- Modern USB 3.2 / USB4 NVMe & SATA Bridges & Card Readers ----
        ("0BDA", "9210", "Realtek (瑞昱)", "RTL9210 / RTL9210B (USB 3.2 NVMe/SATA 桥接)", "常见于 10Gbps 固态硬盘盒与高速固态 U 盘"),
        ("0BDA", "9220", "Realtek (瑞昱)", "RTL9220DP (USB 3.2 Gen2x2 20Gbps 桥接)", "20Gbps 高速 NVMe 桥接主控"),
        ("0BDA", "0328", "Realtek (瑞昱)", "RTS5328 / RTS5329 (USB 3.0 读卡器)", "瑞昱高速 SD/TF 读卡器主控"),
        ("152D", "0562", "JMicron (智微)", "JMS562 (USB 3.0 SATA 桥接)", "智微移动硬盘盒主控"),
        ("152D", "0578", "JMicron (智微)", "JMS578 (USB 3.0 SATA 桥接)", "常见于 SATA 移动硬盘盒"),
        ("152D", "0583", "JMicron (智微)", "JMS583 (USB 3.1 Gen2 NVMe 桥接)", "常见于 10Gbps NVMe 移动固态硬盘盒"),
        ("174C", "1153", "ASMedia (祥硕)", "ASM1153E (USB 3.0 SATA 桥接)", "祥硕经典 SATA 硬盘盒主控"),
        ("174C", "55AA", "ASMedia (祥硕)", "ASM1153E / ASM235CM (USB 3.1 SATA 桥接)", "祥硕高速 SATA 硬盘盒主控"),
        ("174C", "2362", "ASMedia (祥硕)", "ASM2362 (USB 3.1 Gen2 NVMe 桥接)", "10Gbps NVMe 桥接主控"),
        ("174C", "2364", "ASMedia (祥硕)", "ASM2364 (USB 3.2 Gen2x2 20Gbps 桥接)", "20Gbps NVMe 桥接主控"),
        ("174C", "2464", "ASMedia (祥硕)", "ASM2464PD (USB4 / Thunderbolt 4 40Gbps)", "40Gbps USB4 旗舰硬盘盒主控"),
        // ---- Yeestor / SiliconGo / DM / Micov / ITE / Skymedi (from ChipGenius v4.21 dispatcher) ----
        ("BA57", "0001", "Yeestor / SiliconGo (得一微/硅格)", "SG1580 (DM8233) / SG1581 (DM8235)", "得一微/点序 USB 3.0 主控方案"),
        ("23A9", "EF18", "Yeestor / SiliconGo (得一微/硅格)", "SG1581 (DM8235) / YS9082", "得一微/硅格 U 盘主控方案"),
        ("1221", "3234", "Micov / Ameco (迈科微)", "MW8209 / MW8259 / MW8269 / MW8289", "迈科微公版 U 盘主控（常见于廉价/礼品盘，建议核验容量）"),
        ("1307", "0165", "ITE / Usbest (联阳)", "UT165 / IT1167 / IT1170", "联阳经典 U 盘主控"),
        ("1307", "1167", "ITE / Usbest (联阳)", "IT1167 / UT167", "联阳 USB 2.0/3.0 主控"),
        ("13C6", "0000", "Skymedi (擎泰)", "SK6211 / SK6215 / SK6221", "擎泰 U 盘主控"),
        ("1ADA", "0000", "Skymedi (擎泰)", "SK6221 / SK6231 / SK6236A", "擎泰 USB 3.0 主控"),
        ("0DD8", "1000", "Netac / Appotech (朗科)", "Netac OnlyDisk 定制主控", "朗科 U 盘主控方案"),
        ("05E3", "0749", "Genesys Logic (创惟)", "GL3224 (USB 3.0 SD/TF 读卡器)", "创惟主流 USB 3.0 读卡器主控"),
        ("05E3", "0751", "Genesys Logic (创惟)", "GL3232S (USB 3.2 SD/TF 读卡器)", "创惟高速读卡器主控"),
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

/// High-volume 2020-2026 3D NAND Flash IDs not present in 2019 `Chips.wdb`
/// (Kioxia BiCS4/5/6, SanDisk BiCS4/5, Micron B27/B47R/N38A/N48R, YMTC X2/X3, Samsung V6/V7, SK Hynix V6/V7, SpecTek).
fn supplemental_flashes() -> Vec<FlashRecord> {
    let entries: &[(&str, &str, &str)] = &[
        // ---- YMTC (长江存储 Xtacking 2.0 / 3.0) ----
        ("9BC42849", "TLC-16K", "[1CE]YMTC X2-9060 256Gb (128L 3D TLC)"),
        ("9BC52849", "TLC-16K", "[2CE]YMTC X2-9060 512Gb (128L 3D TLC)"),
        ("9BC32849", "TLC-16K", "[1CE]YMTC X2-9060 128Gb (128L 3D TLC)"),
        ("9BC40849", "TLC-16K", "[1CE]YMTC X1-9050 256Gb (64L 3D TLC)"),
        ("9BC52949", "TLC-16K", "[1CE]YMTC X3-9070 512Gb (232L 3D TLC)"),
        ("9BC62949", "TLC-16K", "[1CE]YMTC X3-9070 1Tb (232L 3D TLC)"),
        ("9BC50949", "QLC-16K", "[1CE]YMTC X3-6070 1Tb (232L 3D QLC)"),
        // ---- Toshiba / Kioxia (铠侠 BiCS4 / BiCS5 / BiCS6) ----
        ("983C98B3", "TLC-16K", "[1CE]TH58TFT0T23BA8C / Kioxia BiCS4 256Gb 96L TLC"),
        ("983E98B3", "TLC-16K", "[1CE]TH58TFT1T23BA8H / Kioxia BiCS4 512Gb 96L TLC"),
        ("984898B3", "TLC-16K", "[1CE]Kioxia BiCS5 256Gb (112L 3D TLC)"),
        ("984998B3", "TLC-16K", "[1CE]Kioxia BiCS5 512Gb (112L 3D TLC)"),
        ("984A98B3", "TLC-16K", "[1CE]Kioxia BiCS5 1Tb (112L 3D TLC)"),
        ("983E99B3", "QLC-16K", "[1CE]Kioxia BiCS4/5 1Tb 3D QLC"),
        ("984C98B3", "TLC-16K", "[1CE]Kioxia BiCS6 512Gb/1Tb (162L 3D TLC)"),
        // ---- SanDisk / WD (闪迪 BiCS4 / BiCS5) ----
        ("453C98B3", "TLC-16K", "[1CE]SanDisk BiCS4 256Gb (96L 3D TLC)"),
        ("453E98B3", "TLC-16K", "[1CE]SanDisk BiCS4 512Gb (96L 3D TLC)"),
        ("454898B3", "TLC-16K", "[1CE]SanDisk BiCS5 256Gb (112L 3D TLC)"),
        ("454998B3", "TLC-16K", "[1CE]SanDisk BiCS5 512Gb (112L 3D TLC)"),
        ("454A98B3", "TLC-16K", "[1CE]SanDisk BiCS5 1Tb (112L 3D TLC)"),
        ("453E99B3", "QLC-16K", "[1CE]SanDisk BiCS4/5 1Tb 3D QLC"),
        // ---- Micron (美光 B27A / B27B / B47R / N38A / N48R) ----
        ("2CC30832", "TLC-16K", "[1CE]MT29F256G08EBHAF (Micron B27A 96L TLC)"),
        ("2CC40832", "TLC-16K", "[1CE]MT29F512G08EBHAF (Micron B27B 96L TLC)"),
        ("2CC30832", "TLC-16K", "[1CE]Micron B27A 256Gb 96L TLC"),
        ("2CD30C32", "TLC-16K", "[1CE]Micron B47R 512Gb (176L 3D TLC)"),
        ("2CD40C32", "TLC-16K", "[1CE]Micron B47R 1Tb (176L 3D TLC)"),
        ("2CD31C32", "QLC-16K", "[1CE]Micron N38A 1Tb (96L 3D QLC)"),
        ("2CD31E32", "QLC-16K", "[1CE]Micron N48R 1Tb (176L 3D QLC)"),
        ("2CC50832", "TLC-16K", "[2CE]Micron B27B 1Tb 96L TLC"),
        // ---- SpecTek (美光大S降级/白片 B27 / B47R / N38A) ----
        ("B5C40832", "TLC-16K", "[1CE]SpecTek B27B 512Gb (降级片/大S)"),
        ("B5D30C32", "TLC-16K", "[1CE]SpecTek B47R 512Gb (降级片/大S)"),
        ("B5D31C32", "QLC-16K", "[1CE]SpecTek N38A 1Tb QLC (降级片/大S)"),
        // ---- Samsung (三星 V5 / V6 / V7 3D V-NAND) ----
        ("EC1E98AF", "TLC-16K", "[1CE]K9AFGY8J5B (Samsung V6 128L TLC)"),
        ("EC5E98AF", "TLC-16K", "[1CE]K9BFGY8J5B (Samsung V6/V7 512Gb TLC)"),
        ("EC5E98BF", "TLC-16K", "[1CE]Samsung V7 176L 512Gb TLC"),
        // ---- SK Hynix (海力士 V5 / V6 / V7 3D NAND) ----
        ("AD7E280B", "TLC-16K", "[1CE]H25BFT8A1M8R (Hynix V5 96L 512Gb TLC)"),
        ("AD7E2853", "TLC-16K", "[1CE]H25T2TC88C (Hynix V6 128L 512Gb TLC)"),
        ("AD892853", "TLC-16K", "[1CE]Hynix V7 176L 512Gb TLC"),
        // ---- Intel (英特尔 96L / 144L QLC & TLC) ----
        ("89D3AC32", "QLC-16K", "[1CE]Intel N28A 1Tb (96L 3D QLC)"),
        ("89D3AE32", "QLC-16K", "[1CE]Intel N38A / 144L 1Tb 3D QLC"),
    ];

    entries
        .iter()
        .map(|(fid, ftype, part)| {
            let mfg_byte = u8::from_str_radix(&fid[..2], 16).unwrap_or(0);
            FlashRecord {
                flash_id: (*fid).to_string(),
                manufacturer: nand_manufacturer_name(mfg_byte).to_string(),
                flash_type: (*ftype).to_string(),
                part_numbers: vec![(*part).to_string()],
                is_downgraded: mfg_byte == 0xB5,
            }
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
        assert!(db.flashes.len() >= 1136);

        // Algorithmic fallback for unlisted 2026 YMTC 3D TLC chip
        let algo = decode_flash_id_algorithmic(&[0x9B, 0xC9, 0x28, 0x4A, 0x00, 0x00])
            .expect("YMTC algorithmic decode");
        assert!(algo.manufacturer.contains("YMTC"));
        assert_eq!(algo.flash_type, "TLC-16K");

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
