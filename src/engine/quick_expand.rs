use crate::disk::direct_io::{get_volume_space, AlignedBuffer, DirectFile};
use crate::disk::{format_bytes, format_bytes_decimal, DiskInfo};
use crate::engine::pattern::{fill_buffer, generate_run_id, verify_buffer, VerifyStatus};
use crate::engine::{
    compute_target_bytes, BlockCell, BlockState, EngineContext, SpeedSample, TempWorkspace,
    TestConfig, Verdict,
};
use std::time::Instant;

const MIB: u64 = 1024 * 1024;
const PROBE_SIZE: usize = 1024 * 1024; // 1 MiB sentinel probe per zone

/// Runs MyDiskTest's signature "快速扩容测试" (Quick Fake-Flash Capacity Detection).
///
/// Strategy:
/// 1. Creates FAT32-safe 1 GiB chunks across the target space and tests whether `set_len`
///    physically allocates clusters (true on FAT32/exFAT — 99% of USB flash drives & SD cards).
/// 2. If physical cluster allocation is active:
///    - Allocates chunks across the entire target space rapidly.
///    - Writes unique `run_id + global_offset + checksum` 1 MiB sentinel probes across all grid zones
///      and power-of-two capacity boundaries (512MB, 1GB, 2GB, 4GB, 8GB, 16GB, 32GB, 64GB...).
///    - Flushes to physical media (`F_FULLFSYNC` / `FILE_FLAG_WRITE_THROUGH`).
///    - Reads back every sentinel via unbuffered Direct I/O to catch wrap-around overwriting OR
///      black-hole (0x00/0xFF) discard controllers!
///    - When a boundary fails, performs binary search to pinpoint the exact true flash capacity!
/// 3. If the filesystem is sparse/copy-on-write (APFS / NTFS / ext4 where `set_len` does not
///    consume physical space):
///    - Automatically switches to real physical-write rapid probing across chunks (capped safely
///      or user-configured size) so fake drives can never hide behind sparse file metadata.
pub fn run(ctx: &EngineContext, disk: &DiskInfo, config: &TestConfig) {
    // Prefer true 5-15s Raw LBA lossless probing on external/removable physical disks when enabled!
    if config.use_raw_lba_probe && disk.raw_disk_path.is_some() && !disk.is_system {
        crate::engine::raw_probe::run_raw_lba_probe(ctx, disk, config);
        return;
    }

    let run_id = generate_run_id();
    let target_bytes = match compute_target_bytes(&disk.mount_point, config, 0) {
        Ok(b) => b,
        Err(e) => {
            ctx.log(&e);
            ctx.finish(Verdict::Failed { error: e });
            return;
        }
    };

    let chunk_size = (config.chunk_size_mb.max(64).min(2048)) * MIB;
    let workspace = match TempWorkspace::create(&disk.mount_point, run_id, config.auto_cleanup) {
        Ok(w) => w,
        Err(e) => {
            let msg = format!("无法在目标磁盘创建临时测试目录: {}（请检查是否只读或权限不足）", e);
            ctx.log(&msg);
            ctx.finish(Verdict::Failed { error: msg });
            return;
        }
    };

    ctx.log(format!(
        "开始文件模式快速扩容测试 — 目标空间 {} (设备标称 {})，批次 ID 0x{:08X}",
        format_bytes(target_bytes),
        format_bytes_decimal(disk.physical_bytes.max(disk.total_bytes)),
        run_id & 0xFFFF_FFFF
    ));

    // Benchmark a single 16 MiB set_len() call to detect:
    // 1) Fast metadata cluster allocation (< 80ms AND consumes physical free space), vs
    // 2) Kernel synchronous zero-fill (macOS exFAT/FAT32 takes hundreds of ms for 16 MiB -> NEVER call set_len on 128 GB!), vs
    // 3) Sparse / CoW allocation (APFS/NTFS consumes ~0 bytes).
    let probe_sparse_size = (16 * MIB).min(target_bytes);
    let (_, free_before) = get_volume_space(&disk.mount_point).unwrap_or((0, 0));
    let sparse_test_path = workspace.dir.join("alloc_probe.bin");
    let mut direct_io_active = false;
    let mut fast_physical_prealloc = false;
    let mut kernel_zero_fill_detected = false;

    if let Ok(mut f) = DirectFile::create_rw(&sparse_test_path) {
        direct_io_active = f.unbuffered_active;
        let t0 = Instant::now();
        if f.set_len(probe_sparse_size).is_ok() {
            let _ = f.flush_to_media();
            let alloc_ms = t0.elapsed().as_millis();
            let (_, free_after) = get_volume_space(&disk.mount_point).unwrap_or((0, 0));
            let consumed = free_before.saturating_sub(free_after);
            if consumed >= probe_sparse_size / 2 {
                if alloc_ms < 80 {
                    fast_physical_prealloc = true;
                } else {
                    kernel_zero_fill_detected = true;
                }
            }
        }
    }
    let _ = std::fs::remove_file(&sparse_test_path);

    ctx.update(|s| {
        s.direct_io_active = direct_io_active;
        s.target_bytes = target_bytes;
    });

    if fast_physical_prealloc {
        ctx.log("检测到高速实体簇链分配，启用【极速跨区文件探针模式】");
        run_fast_prealloc_probe(ctx, disk, config, &workspace, run_id, target_bytes, chunk_size);
    } else {
        // Never silently zero-fill 128 GB in "Quick" file mode!
        // Cap auto-size to 512 MiB so file-mode quick check finishes in seconds, and guide user to Raw LBA mode for full-disk 10s probing.
        let effective_bytes = if config.target_size_mb == 0 {
            target_bytes.min(512 * MIB)
        } else {
            target_bytes
        };
        if kernel_zero_fill_detected {
            ctx.log(format!(
                "提示：当前系统 ({}) 的文件分配会自动写零填充（避免耗时过长，文件模式快速验证已限定为 {}）。如需 5~15 秒覆盖整盘全容量，请勾选「⚡ 极速底层 LBA 探针」！",
                disk.filesystem,
                format_bytes(effective_bytes)
            ));
        } else {
            ctx.log(format!(
                "检测到稀疏/写时复制文件系统（{}），为防止稀疏分配漏检，自动切换为【实体写入校验模式】（测试 {}；如需全盘覆盖请使用「⚡ 极速底层 LBA 探针」或「全盘完整性校验」）",
                disk.filesystem,
                format_bytes(effective_bytes)
            ));
        }
        run_real_write_quick_check(
            ctx,
            disk,
            config,
            &workspace,
            run_id,
            effective_bytes,
            chunk_size,
        );
    }

    ctx.update(|s| {
        s.phase_label = "正在自动清理临时测试文件...".to_string();
    });
    workspace.cleanup_now();
}

fn run_fast_prealloc_probe(
    ctx: &EngineContext,
    disk: &DiskInfo,
    config: &TestConfig,
    workspace: &TempWorkspace,
    run_id: u64,
    target_bytes: u64,
    chunk_size: u64,
) {
    let num_chunks = ((target_bytes + chunk_size - 1) / chunk_size) as usize;
    let grid_count = config.grid_blocks.clamp(64, 400).min((target_bytes / (PROBE_SIZE as u64)).max(1) as usize);
    let cell_span = (target_bytes / grid_count as u64).max(PROBE_SIZE as u64);

    let cells: Vec<BlockCell> = (0..grid_count)
        .map(|i| {
            let offset = (i as u64) * cell_span;
            let size = cell_span.min(target_bytes.saturating_sub(offset));
            BlockCell {
                index: i,
                state: BlockState::Untested,
                offset_bytes: offset,
                size_bytes: size,
                write_mb_s: 0.0,
                read_mb_s: 0.0,
                note: None,
            }
        })
        .collect();

    ctx.update(|s| {
        s.blocks = cells;
        s.phase_label = format!("阶段 1/3：分配全盘空间簇链（共 {} 个分片文件）...", num_chunks);
    });

    // Step 1: Allocate all chunk files across the target capacity
    let mut chunk_sizes = Vec::with_capacity(num_chunks);
    let mut remaining = target_bytes;
    for idx in 0..num_chunks {
        if ctx.is_cancelled() {
            ctx.finish(Verdict::Cancelled {
                summary: "测试已由用户手动停止，临时文件已清理。".to_string(),
            });
            return;
        }
        let this_size = remaining.min(chunk_size);
        remaining = remaining.saturating_sub(this_size);
        chunk_sizes.push(this_size);

        let path = workspace.chunk_path(idx);
        let f = match DirectFile::create_rw(&path) {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("创建分片文件 #{} 失败: {}", idx, e);
                ctx.log(&msg);
                ctx.finish(Verdict::Failed { error: msg });
                return;
            }
        };
        if let Err(e) = f.set_len(this_size) {
            let allocated_so_far = (idx as u64) * chunk_size;
            ctx.log(format!(
                "分配至 {} 时失败: {}（可能空间不足或设备异常）",
                format_bytes(allocated_so_far),
                e
            ));
            ctx.finish(Verdict::Failed {
                error: format!("空间分配失败: {}", e),
            });
            return;
        }
        ctx.update(|s| {
            s.progress = 0.15 * ((idx + 1) as f32 / num_chunks as f32);
        });
    }

    // Step 2: Write unique 1 MiB sentinel blocks at every grid zone offset
    ctx.update(|s| {
        s.phase_label = format!("阶段 2/3：向全盘 {} 个采样区写入唯一指纹探针...", grid_count);
    });

    let mut wbuf = AlignedBuffer::new(PROBE_SIZE);
    let mut total_written: u64 = 0;
    let write_start = Instant::now();
    let mut open_chunk_idx: Option<usize> = None;
    let mut open_file: Option<DirectFile> = None;

    for cell_idx in 0..grid_count {
        if ctx.is_cancelled() {
            ctx.finish(Verdict::Cancelled {
                summary: "快速扩容测试已取消，临时文件已清理。".to_string(),
            });
            return;
        }

        let offset = ((cell_idx as u64) * cell_span / 4096) * 4096;
        let c_idx = (offset / chunk_size) as usize;
        let local_off = offset % chunk_size;
        if c_idx >= chunk_sizes.len() || local_off + (PROBE_SIZE as u64) > chunk_sizes[c_idx] {
            continue;
        }

        if open_chunk_idx != Some(c_idx) {
            if let Some(mut prev) = open_file.take() {
                let _ = prev.flush_to_media();
            }
            match DirectFile::open_rw(&workspace.chunk_path(c_idx)) {
                Ok(f) => {
                    open_file = Some(f);
                    open_chunk_idx = Some(c_idx);
                }
                Err(e) => {
                    ctx.log(format!("打开分片 #{} 写入失败: {}", c_idx, e));
                    ctx.update(|s| {
                        s.blocks[cell_idx].state = BlockState::Bad;
                        s.blocks[cell_idx].note = Some(format!("I/O 错误: {}", e));
                    });
                    continue;
                }
            }
        }

        fill_buffer(&mut wbuf, run_id, offset);
        let t0 = Instant::now();
        let res = open_file.as_mut().unwrap().write_at(local_off, &wbuf);
        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (PROBE_SIZE as f64 / (1024.0 * 1024.0)) / dt;

        match res {
            Ok(()) => {
                total_written += PROBE_SIZE as u64;
                let avg_w = (total_written as f64 / (1024.0 * 1024.0))
                    / write_start.elapsed().as_secs_f64().max(1e-6);
                ctx.update(|s| {
                    s.bytes_written = total_written;
                    s.current_speed_mb_s = mb_s;
                    s.avg_write_mb_s = avg_w;
                    s.progress = 0.15 + 0.40 * ((cell_idx + 1) as f32 / grid_count as f32);
                    s.blocks[cell_idx].state = BlockState::Written;
                    s.blocks[cell_idx].write_mb_s = mb_s as f32;
                    s.write_curve.push(SpeedSample {
                        x: offset as f64 / (1024.0 * 1024.0 * 1024.0),
                        mb_per_sec: mb_s,
                    });
                });
            }
            Err(e) => {
                ctx.log(format!(
                    "偏移 {} 处写入失败: {}",
                    format_bytes(offset),
                    e
                ));
                ctx.update(|s| {
                    s.blocks[cell_idx].state = BlockState::Bad;
                    s.blocks[cell_idx].note = Some(format!("写入失败: {}", e));
                });
            }
        }
    }

    if let Some(mut f) = open_file.take() {
        let _ = f.flush_to_media();
    }
    open_chunk_idx = None;

    // Step 3: Unbuffered read-back verification of every sentinel!
    // Reading from offset 0 upward immediately catches wrap-around overwrites AND high-offset black holes!
    ctx.update(|s| {
        s.phase_label = format!("阶段 3/3：绕过系统缓存回读校验全部 {} 个探针...", grid_count);
    });

    let mut rbuf = AlignedBuffer::new(PROBE_SIZE);
    let mut total_verified: u64 = 0;
    let read_start = Instant::now();
    let mut first_wrap_offset: Option<u64> = None;
    let mut first_bad_offset: Option<u64> = None;
    let mut last_good_offset: u64 = 0;
    let mut wrap_reason = String::new();
    let mut zero_or_ff_hits: usize = 0;

    for cell_idx in 0..grid_count {
        if ctx.is_cancelled() {
            ctx.finish(Verdict::Cancelled {
                summary: "快速扩容测试已取消，临时文件已清理。".to_string(),
            });
            return;
        }

        let offset = ((cell_idx as u64) * cell_span / 4096) * 4096;
        let c_idx = (offset / chunk_size) as usize;
        let local_off = offset % chunk_size;
        if c_idx >= chunk_sizes.len() || local_off + (PROBE_SIZE as u64) > chunk_sizes[c_idx] {
            continue;
        }

        if open_chunk_idx != Some(c_idx) {
            open_file = DirectFile::open_read(&workspace.chunk_path(c_idx)).ok();
            open_chunk_idx = Some(c_idx);
        }

        let t0 = Instant::now();
        let read_res = match open_file.as_mut() {
            Some(f) => f.read_at(local_off, &mut rbuf),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "无法重新打开分片文件",
            )),
        };
        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (PROBE_SIZE as f64 / (1024.0 * 1024.0)) / dt;

        let (new_state, note) = match read_res {
            Ok(()) => {
                total_verified += PROBE_SIZE as u64;
                match verify_buffer(&rbuf, run_id, offset) {
                    VerifyStatus::Ok => {
                        if first_bad_offset.is_none() && first_wrap_offset.is_none() {
                            last_good_offset = offset + cell_span;
                        }
                        if mb_s < config.slow_threshold_mb_s {
                            (
                                BlockState::Slow,
                                Some(format!("校验通过，但读取偏慢 ({:.1} MB/s)", mb_s)),
                            )
                        } else {
                            (BlockState::Good, None)
                        }
                    }
                    VerifyStatus::Wrapped {
                        expected_offset,
                        found_offset,
                    } => {
                        let diff = found_offset.abs_diff(expected_offset);
                        if first_wrap_offset.is_none() {
                            first_wrap_offset = Some(if diff > 0 { diff } else { offset });
                            wrap_reason = format!(
                                "在偏移 {} 读到了偏移 {} 的数据（循环覆盖回绕，典型扩容盘特征！）",
                                format_bytes(expected_offset),
                                format_bytes(found_offset)
                            );
                            ctx.log(format!("🚨 发现扩容回绕：{}", wrap_reason));
                        }
                        (
                            BlockState::Wrapped,
                            Some(format!(
                                "扩容回绕：期望 {} 实读 {}",
                                format_bytes(expected_offset),
                                format_bytes(found_offset)
                            )),
                        )
                    }
                    VerifyStatus::ZeroFilled => {
                        zero_or_ff_hits += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(offset);
                            ctx.log(format!(
                                "🚨 偏移 {} 起读回全 0x00（典型黑洞/丢弃型扩容盘特征）",
                                format_bytes(offset)
                            ));
                        }
                        (BlockState::Bad, Some("读回全 0x00（疑似假容量黑洞区）".to_string()))
                    }
                    VerifyStatus::FFFilled => {
                        zero_or_ff_hits += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(offset);
                            ctx.log(format!(
                                "🚨 偏移 {} 起读回全 0xFF（未写入/假容量闪存区）",
                                format_bytes(offset)
                            ));
                        }
                        (BlockState::Bad, Some("读回全 0xFF（疑似假容量未映射区）".to_string()))
                    }
                    VerifyStatus::Corrupted { reason } => {
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(offset);
                        }
                        (BlockState::Bad, Some(reason))
                    }
                }
            }
            Err(e) => {
                if first_bad_offset.is_none() {
                    first_bad_offset = Some(offset);
                }
                (BlockState::Bad, Some(format!("读取失败: {}", e)))
            }
        };

        let avg_r = (total_verified as f64 / (1024.0 * 1024.0))
            / read_start.elapsed().as_secs_f64().max(1e-6);
        ctx.update(|s| {
            s.bytes_verified = total_verified;
            s.current_speed_mb_s = mb_s;
            s.avg_read_mb_s = avg_r;
            s.progress = 0.55 + 0.45 * ((cell_idx + 1) as f32 / grid_count as f32);
            s.blocks[cell_idx].state = new_state;
            s.blocks[cell_idx].read_mb_s = mb_s as f32;
            s.blocks[cell_idx].note = note;
            s.read_curve.push(SpeedSample {
                x: offset as f64 / (1024.0 * 1024.0 * 1024.0),
                mb_per_sec: mb_s,
            });
        });
    }

    drop(open_file);
    finalize_expand_verdict(
        ctx,
        disk,
        target_bytes,
        first_wrap_offset,
        first_bad_offset,
        last_good_offset,
        zero_or_ff_hits,
        wrap_reason,
    );
}

/// Real-write quick expand mode for sparse/CoW filesystems (APFS, NTFS, ext4) where `set_len`
/// doesn't physically allocate blocks. Writes real unbuffered blocks and verifies every single one.
fn run_real_write_quick_check(
    ctx: &EngineContext,
    disk: &DiskInfo,
    config: &TestConfig,
    workspace: &TempWorkspace,
    run_id: u64,
    effective_bytes: u64,
    chunk_size: u64,
) {
    let io_size: usize = 1024 * 1024; // 1 MiB
    let total_steps = (effective_bytes / io_size as u64).max(1);
    let grid_count = config.grid_blocks.clamp(64, 400).min(total_steps as usize);
    let cell_bytes = (effective_bytes / grid_count as u64).max(io_size as u64);

    let cells: Vec<BlockCell> = (0..grid_count)
        .map(|i| {
            let offset = (i as u64) * cell_bytes;
            let size = cell_bytes.min(effective_bytes.saturating_sub(offset));
            BlockCell {
                index: i,
                state: BlockState::Untested,
                offset_bytes: offset,
                size_bytes: size,
                write_mb_s: 0.0,
                read_mb_s: 0.0,
                note: None,
            }
        })
        .collect();

    ctx.update(|s| {
        s.target_bytes = effective_bytes;
        s.blocks = cells;
        s.phase_label = "阶段 1/2：实体无缓冲写入校验数据...".to_string();
    });

    let mut wbuf = AlignedBuffer::new(io_size);
    let mut written_total: u64 = 0;
    let write_start = Instant::now();
    let mut chunk_idx = 0usize;
    let mut file = match DirectFile::create_rw(&workspace.chunk_path(0)) {
        Ok(f) => f,
        Err(e) => {
            ctx.finish(Verdict::Failed {
                error: format!("创建测试文件失败: {}", e),
            });
            return;
        }
    };
    let mut local_written: u64 = 0;
    let mut last_curve_time = Instant::now();

    while written_total < effective_bytes {
        if ctx.is_cancelled() {
            ctx.finish(Verdict::Cancelled {
                summary: "测试已取消，临时文件已清理。".to_string(),
            });
            return;
        }
        if local_written >= chunk_size {
            let _ = file.flush_to_media();
            chunk_idx += 1;
            local_written = 0;
            file = match DirectFile::create_rw(&workspace.chunk_path(chunk_idx)) {
                Ok(f) => f,
                Err(e) => {
                    ctx.finish(Verdict::Failed {
                        error: format!("创建分片文件 #{} 失败: {}", chunk_idx, e),
                    });
                    return;
                }
            };
        }

        fill_buffer(&mut wbuf, run_id, written_total);
        let t0 = Instant::now();
        if let Err(e) = file.write_all_aligned(&wbuf) {
            ctx.log(format!("偏移 {} 写入出错: {}", format_bytes(written_total), e));
            let c = ((written_total / cell_bytes) as usize).min(grid_count - 1);
            ctx.update(|s| {
                s.blocks[c].state = BlockState::Bad;
                s.blocks[c].note = Some(format!("写入失败: {}", e));
            });
            break;
        }
        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (io_size as f64 / (1024.0 * 1024.0)) / dt;

        let cell_idx = ((written_total / cell_bytes) as usize).min(grid_count - 1);
        written_total += io_size as u64;
        local_written += io_size as u64;

        let elapsed = write_start.elapsed().as_secs_f64().max(1e-6);
        let avg_w = (written_total as f64 / (1024.0 * 1024.0)) / elapsed;
        let emit_curve = last_curve_time.elapsed().as_millis() >= 120 || written_total == effective_bytes;
        if emit_curve {
            last_curve_time = Instant::now();
        }
        ctx.update(|s| {
            s.bytes_written = written_total;
            s.current_speed_mb_s = mb_s;
            s.avg_write_mb_s = avg_w;
            s.progress = 0.5 * (written_total as f32 / effective_bytes as f32);
            if s.blocks[cell_idx].state == BlockState::Untested {
                s.blocks[cell_idx].state = BlockState::Written;
            }
            s.blocks[cell_idx].write_mb_s = mb_s as f32;
            if emit_curve {
                s.write_curve.push(SpeedSample {
                    x: written_total as f64 / (1024.0 * 1024.0 * 1024.0),
                    mb_per_sec: mb_s,
                });
            }
        });
    }
    let _ = file.flush_to_media();
    drop(file);

    // Phase 2: Read back & verify
    ctx.update(|s| {
        s.phase_label = "阶段 2/2：无缓冲回读校验数据完整性...".to_string();
    });

    let mut rbuf = AlignedBuffer::new(io_size);
    let mut verified_total: u64 = 0;
    let read_start = Instant::now();
    let mut first_wrap_offset: Option<u64> = None;
    let mut first_bad_offset: Option<u64> = None;
    let mut last_good_offset: u64 = 0;
    let mut zero_or_ff_hits: usize = 0;
    let mut wrap_reason = String::new();
    let mut last_curve_time = Instant::now();

    for c_idx in 0..=chunk_idx {
        let mut rf = match DirectFile::open_read(&workspace.chunk_path(c_idx)) {
            Ok(f) => f,
            Err(e) => {
                ctx.log(format!("打开分片 #{} 回读失败: {}", c_idx, e));
                break;
            }
        };
        let mut local_read: u64 = 0;
        while local_read < chunk_size && verified_total < written_total {
            if ctx.is_cancelled() {
                ctx.finish(Verdict::Cancelled {
                    summary: "测试已取消，临时文件已清理。".to_string(),
                });
                return;
            }
            let t0 = Instant::now();
            let res = rf.read_exact_aligned(&mut rbuf);
            let dt = t0.elapsed().as_secs_f64().max(1e-6);
            let mb_s = (io_size as f64 / (1024.0 * 1024.0)) / dt;
            let cell_idx = ((verified_total / cell_bytes) as usize).min(grid_count - 1);

            let (st, note) = match res {
                Ok(()) => match verify_buffer(&rbuf, run_id, verified_total) {
                    VerifyStatus::Ok => {
                        if first_bad_offset.is_none() && first_wrap_offset.is_none() {
                            last_good_offset = verified_total + io_size as u64;
                        }
                        if mb_s < config.slow_threshold_mb_s {
                            (BlockState::Slow, Some(format!("读取偏慢 ({:.1} MB/s)", mb_s)))
                        } else {
                            (BlockState::Good, None)
                        }
                    }
                    VerifyStatus::Wrapped {
                        expected_offset,
                        found_offset,
                    } => {
                        if first_wrap_offset.is_none() {
                            first_wrap_offset = Some(found_offset.abs_diff(expected_offset));
                            wrap_reason = format!(
                                "偏移 {} 读到了偏移 {} 的数据（循环覆盖扩容）",
                                format_bytes(expected_offset),
                                format_bytes(found_offset)
                            );
                            ctx.log(format!("🚨 {}", wrap_reason));
                        }
                        (BlockState::Wrapped, Some("扩容回绕".to_string()))
                    }
                    VerifyStatus::ZeroFilled => {
                        zero_or_ff_hits += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(verified_total);
                        }
                        (BlockState::Bad, Some("全 0x00 异常".to_string()))
                    }
                    VerifyStatus::FFFilled => {
                        zero_or_ff_hits += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(verified_total);
                        }
                        (BlockState::Bad, Some("全 0xFF 异常".to_string()))
                    }
                    VerifyStatus::Corrupted { reason } => {
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(verified_total);
                        }
                        (BlockState::Bad, Some(reason))
                    }
                },
                Err(e) => {
                    if first_bad_offset.is_none() {
                        first_bad_offset = Some(verified_total);
                    }
                    (BlockState::Bad, Some(format!("读取错误: {}", e)))
                }
            };

            verified_total += io_size as u64;
            local_read += io_size as u64;
            let avg_r = (verified_total as f64 / (1024.0 * 1024.0))
                / read_start.elapsed().as_secs_f64().max(1e-6);
            let emit_curve = last_curve_time.elapsed().as_millis() >= 120 || verified_total >= written_total;
            if emit_curve {
                last_curve_time = Instant::now();
            }

            ctx.update(|s| {
                s.bytes_verified = verified_total;
                s.current_speed_mb_s = mb_s;
                s.avg_read_mb_s = avg_r;
                s.progress = 0.5 + 0.5 * (verified_total as f32 / written_total.max(1) as f32);
                // Keep worse status if cell already saw bad/wrapped
                let cur = s.blocks[cell_idx].state;
                if !matches!(cur, BlockState::Bad | BlockState::Wrapped) {
                    s.blocks[cell_idx].state = st;
                    if note.is_some() {
                        s.blocks[cell_idx].note = note;
                    }
                }
                s.blocks[cell_idx].read_mb_s = mb_s as f32;
                if emit_curve {
                    s.read_curve.push(SpeedSample {
                        x: verified_total as f64 / (1024.0 * 1024.0 * 1024.0),
                        mb_per_sec: mb_s,
                    });
                }
            });
        }
    }

    finalize_expand_verdict(
        ctx,
        disk,
        written_total,
        first_wrap_offset,
        first_bad_offset,
        last_good_offset,
        zero_or_ff_hits,
        wrap_reason,
    );
}

#[allow(clippy::too_many_arguments)]
fn finalize_expand_verdict(
    ctx: &EngineContext,
    disk: &DiskInfo,
    tested_bytes: u64,
    first_wrap_offset: Option<u64>,
    first_bad_offset: Option<u64>,
    last_good_offset: u64,
    zero_or_ff_hits: usize,
    wrap_reason: String,
) {
    let (good, slow, bad, wrapped) = ctx
        .state
        .lock()
        .map(|s| s.count_blocks())
        .unwrap_or((0, 0, 0, 0));

    let reported = disk.physical_bytes.max(disk.total_bytes);

    if wrapped > 0 || (zero_or_ff_hits >= 3 && bad >= 3) {
        let est_real = first_wrap_offset
            .or(first_bad_offset)
            .unwrap_or(last_good_offset)
            .min(tested_bytes);
        let reason = if !wrap_reason.is_empty() {
            wrap_reason
        } else {
            format!(
                "从约 {} 起连续读回全 0x00 / 0xFF，共 {} 个异常区，高度疑似黑洞型扩容盘！",
                format_bytes(est_real),
                zero_or_ff_hits
            )
        };
        ctx.log(format!(
            "❌ 结论：疑似扩容盘！标称 {}，实际可用真实容量约 {}",
            format_bytes_decimal(reported),
            format_bytes(est_real)
        ));
        ctx.finish(Verdict::FakeCapacity {
            reported_bytes: reported,
            estimated_real_bytes: est_real,
            reason,
        });
    } else if bad > 0 {
        let summary = format!(
            "快速检测完成：正常 {} 块，慢速 {} 块，损坏 {} 块。建议运行「全盘完整性校验」进一步排查。",
            good, slow, bad
        );
        ctx.log(&summary);
        ctx.finish(Verdict::BadBlocks {
            verified_bytes: tested_bytes,
            bad_count: bad,
            slow_count: slow,
            summary,
        });
    } else {
        let summary = format!(
            "快速扩容检测通过！覆盖测试 {}，全部 {} 个检测区校验一致，未发现扩容回绕或黑洞区（慢速区 {} 个）。",
            format_bytes(tested_bytes),
            good + slow,
            slow
        );
        ctx.log(format!("✅ {}", summary));
        ctx.finish(Verdict::Healthy {
            verified_bytes: tested_bytes,
            summary,
        });
    }
}
