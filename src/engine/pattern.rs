/// Sector size used for verifiable data blocks (matches 4K physical sectors & Direct I/O alignment).
pub const SECTOR_SIZE: usize = 4096;
const MAGIC: u64 = 0x2136_3230_3254_444D; // "MDT2026!" in little-endian

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyStatus {
    Ok,
    ZeroFilled,
    FFFilled,
    /// Valid MyDiskTest sector from the SAME run, but written for a DIFFERENT offset -> Wrap-around fake drive!
    Wrapped {
        expected_offset: u64,
        found_offset: u64,
    },
    /// Bit-flips, bad NAND cells, foreign data, or checksum failure.
    Corrupted {
        reason: String,
    },
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Fills a 4096-byte sector with a deterministic, non-compressible, self-verifying pattern
/// containing `run_id`, `logical_offset`, PRNG payload, and a 64-bit integrity checksum.
pub fn fill_sector(buf: &mut [u8], run_id: u64, logical_offset: u64) {
    debug_assert_eq!(buf.len(), SECTOR_SIZE);

    buf[0..8].copy_from_slice(&MAGIC.to_le_bytes());
    buf[8..16].copy_from_slice(&run_id.to_le_bytes());
    buf[16..24].copy_from_slice(&logical_offset.to_le_bytes());

    let mut state = run_id
        ^ logical_offset.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ 0xA076_1D64_78BD_642F;

    // Fast checksum accumulated while generating 64-bit payload words (4..512).
    let mut hash: u64 = MAGIC ^ run_id.rotate_left(13) ^ logical_offset.rotate_right(7);

    let mut i = 32;
    while i < SECTOR_SIZE {
        let word = splitmix64(&mut state);
        buf[i..i + 8].copy_from_slice(&word.to_le_bytes());
        hash = hash.wrapping_mul(0x1000_0000_01B3).wrapping_add(word ^ (i as u64));
        i += 8;
    }

    // Store final integrity checksum in header [24..32].
    buf[24..32].copy_from_slice(&hash.to_le_bytes());
}

/// Fills an aligned multi-sector buffer starting at `base_offset`.
pub fn fill_buffer(buf: &mut [u8], run_id: u64, base_offset: u64) {
    debug_assert_eq!(buf.len() % SECTOR_SIZE, 0);
    for (idx, sector) in buf.chunks_exact_mut(SECTOR_SIZE).enumerate() {
        let offset = base_offset + (idx as u64) * (SECTOR_SIZE as u64);
        fill_sector(sector, run_id, offset);
    }
}

/// Verifies a single 4096-byte sector against `run_id` and `expected_offset`.
pub fn verify_sector(buf: &[u8], run_id: u64, expected_offset: u64) -> VerifyStatus {
    debug_assert_eq!(buf.len(), SECTOR_SIZE);

    let magic = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let found_run_id = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    let found_offset = u64::from_le_bytes(buf[16..24].try_into().unwrap());
    let stored_hash = u64::from_le_bytes(buf[24..32].try_into().unwrap());

    if magic != MAGIC {
        if buf.iter().all(|&b| b == 0x00) {
            return VerifyStatus::ZeroFilled;
        }
        if buf.iter().all(|&b| b == 0xFF) {
            return VerifyStatus::FFFilled;
        }
        return VerifyStatus::Corrupted {
            reason: format!("扇区魔数异常 (0x{:016X})，数据损坏或读取异常", magic),
        };
    }

    if found_run_id != run_id {
        return VerifyStatus::Corrupted {
            reason: format!(
                "批次 ID 不匹配 (期望 0x{:X}, 读到 0x{:X})，疑似残留旧数据或未实际写入",
                run_id, found_run_id
            ),
        };
    }

    if found_offset != expected_offset {
        return VerifyStatus::Wrapped {
            expected_offset,
            found_offset,
        };
    }

    // Recompute payload + checksum to catch single-bit NAND flips!
    let mut state = run_id
        ^ expected_offset.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ 0xA076_1D64_78BD_642F;
    let mut hash: u64 = MAGIC ^ run_id.rotate_left(13) ^ expected_offset.rotate_right(7);

    let mut i = 32;
    while i < SECTOR_SIZE {
        let expected_word = splitmix64(&mut state);
        let actual_word = u64::from_le_bytes(buf[i..i + 8].try_into().unwrap());
        if actual_word != expected_word {
            return VerifyStatus::Corrupted {
                reason: format!("偏移 0x{:X} 处字节 +{} 发生位翻转/数据校验失败", expected_offset, i),
            };
        }
        hash = hash
            .wrapping_mul(0x1000_0000_01B3)
            .wrapping_add(actual_word ^ (i as u64));
        i += 8;
    }

    if stored_hash != hash {
        return VerifyStatus::Corrupted {
            reason: format!("偏移 0x{:X} 处校验和不匹配", expected_offset),
        };
    }

    VerifyStatus::Ok
}

/// Verifies an entire multi-sector buffer. Returns the first non-Ok status encountered, or Ok.
pub fn verify_buffer(buf: &[u8], run_id: u64, base_offset: u64) -> VerifyStatus {
    debug_assert_eq!(buf.len() % SECTOR_SIZE, 0);
    for (idx, sector) in buf.chunks_exact(SECTOR_SIZE).enumerate() {
        let offset = base_offset + (idx as u64) * (SECTOR_SIZE as u64);
        let st = verify_sector(sector, run_id, offset);
        if st != VerifyStatus::Ok {
            return st;
        }
    }
    VerifyStatus::Ok
}

/// Generates a unique 64-bit run identifier for each test execution.
pub fn generate_run_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x1234_5678_9ABC_DEF0);
    let mut s = nanos ^ ((&nanos as *const u64) as u64);
    splitmix64(&mut s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sector_roundtrip_and_tamper_detection() {
        let run_id = generate_run_id();
        let mut buf = vec![0u8; SECTOR_SIZE];
        fill_sector(&mut buf, run_id, 4096 * 42);
        assert_eq!(verify_sector(&buf, run_id, 4096 * 42), VerifyStatus::Ok);

        // Wrap-around detection: reading same sector at a different expected offset
        assert_eq!(
            verify_sector(&buf, run_id, 0),
            VerifyStatus::Wrapped {
                expected_offset: 0,
                found_offset: 4096 * 42
            }
        );

        // Single bit-flip detection
        buf[512] ^= 0x01;
        assert!(matches!(
            verify_sector(&buf, run_id, 4096 * 42),
            VerifyStatus::Corrupted { .. }
        ));

        // Zero-fill detection
        buf.fill(0x00);
        assert_eq!(verify_sector(&buf, run_id, 0), VerifyStatus::ZeroFilled);

        // 0xFF-fill detection
        buf.fill(0xFF);
        assert_eq!(verify_sector(&buf, run_id, 0), VerifyStatus::FFFilled);
    }
}
