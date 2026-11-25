use crate::disk::direct_io::{AlignedBuffer, DirectFile};
use crate::disk::{format_bytes, format_bytes_decimal, DiskInfo};
use crate::engine::pattern::{fill_buffer, generate_run_id, verify_buffer, VerifyStatus};
use crate::engine::{
    compute_target_bytes, BlockCell, BlockState, EngineContext, SpeedSample, TempWorkspace,
    TestConfig, Verdict,
};
use std::time::Instant;

const MIB: u64 = 1024 * 1024;
const IO_BLOCK: usize = 1024 * 1024; // 1 MiB aligned sequential I/O

/// Full Data Integrity Verification & Bad Block Scan (全盘数据完整性校验 & 坏块扫描).
/// Writes deterministic verifiable 4KB sectors across the entire target capacity in FAT32-safe
/// chunks using unbuffered Direct I/O, then reads every single sector back to detect fake
/// capacity, bad flash blocks, bit rot, and slow/dying cells.
pub fn run(ctx: &EngineContext, disk: &DiskInfo, config: &TestConfig) {
    let run_id = generate_run_id();
    let target_bytes = match compute_target_bytes(&disk.mount_point, config, 0) {
        Ok(b) => b,
        Err(e) => {
            ctx.log(&e);
            ctx.finish(Verdict::Failed { error: e });
            return;
        }
    };

    let chunk_size = config.chunk_size_mb.clamp(64, 2048) * MIB;
    let workspace = match TempWorkspace::create(&disk.mount_point, run_id, config.auto_cleanup) {
        Ok(w) => w,
        Err(e) => {
            let msg = format!("无法创建临时测试目录: {}", e);
            ctx.log(&msg);
            ctx.finish(Verdict::Failed { error: msg });
            return;
        }
    };

    let total_mibs = (target_bytes / MIB).max(1);
    let grid_count = config.grid_blocks.clamp(64, 900).min(total_mibs as usize);
    let cell_bytes = (target_bytes / grid_count as u64).max(MIB);

    let cells: Vec<BlockCell> = (0..grid_count)
        .map(|i| {
            let offset = (i as u64) * cell_bytes;
            let size = cell_bytes.min(target_bytes.saturating_sub(offset));
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
        s.target_bytes = target_bytes;
        s.blocks = cells;
        s.phase_label = "阶段 1/2：全盘无缓冲顺序写入校验数据...".to_string();
    });

    ctx.log(format!(
        "开始全盘完整性校验与坏块扫描 — 目标 {}，分片 {}，网格 {} 块，批次 ID 0x{:08X}",
        format_bytes(target_bytes),
        format_bytes(chunk_size),
        grid_count,
        run_id & 0xFFFF_FFFF
    ));

    // Phase 1: Full unbuffered write
    let mut wbuf = AlignedBuffer::new(IO_BLOCK);
    let mut written_total: u64 = 0;
    let mut chunk_written_sizes: Vec<u64> = Vec::new();
    let write_start = Instant::now();
    let mut last_curve = Instant::now();
    let mut write_io_errors: usize = 0;

    let mut chunk_idx = 0usize;
    let mut current_file: Option<DirectFile> = None;
    let mut local_written: u64 = 0;

    while written_total < target_bytes {
        if ctx.is_cancelled() {
            drop(current_file);
            workspace.cleanup_now();
            ctx.finish(Verdict::Cancelled {
                summary: "全盘校验已取消，所有临时文件已安全清理。".to_string(),
            });
            return;
        }

        if current_file.is_none() || local_written >= chunk_size {
            if let Some(mut f) = current_file.take() {
                let _ = f.flush_to_media();
                chunk_written_sizes.push(local_written);
                chunk_idx += 1;
            }
            local_written = 0;
            match DirectFile::create_rw(&workspace.chunk_path(chunk_idx)) {
                Ok(f) => {
                    if chunk_idx == 0 {
                        let active = f.unbuffered_active;
                        ctx.update(|s| s.direct_io_active = active);
                        ctx.log(if active {
                            "已启用底层 Direct I/O 无缓冲直写直读（已绕过系统内存缓存）"
                        } else {
                            "提示：当前文件系统不支持完全无缓冲标志，已启用同步落盘模式"
                        });
                    }
                    current_file = Some(f);
                }
                Err(e) => {
                    ctx.log(format!("创建分片 #{} 失败: {}", chunk_idx, e));
                    break;
                }
            }
        }

        let cell_idx = ((written_total / cell_bytes) as usize).min(grid_count - 1);
        ctx.update(|s| {
            if s.blocks[cell_idx].state == BlockState::Untested {
                s.blocks[cell_idx].state = BlockState::Writing;
            }
        });

        fill_buffer(&mut wbuf, run_id, written_total);
        let t0 = Instant::now();
        let res = current_file.as_mut().unwrap().write_all_aligned(&wbuf);
        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (IO_BLOCK as f64 / (1024.0 * 1024.0)) / dt;

        match res {
            Ok(()) => {
                written_total += IO_BLOCK as u64;
                local_written += IO_BLOCK as u64;
                let elapsed = write_start.elapsed().as_secs_f64().max(1e-6);
                let avg_w = (written_total as f64 / (1024.0 * 1024.0)) / elapsed;
                let remaining_mb = (target_bytes.saturating_sub(written_total)) as f64 / (1024.0 * 1024.0);
                // Estimate total remaining = remaining write + rough read phase
                let eta = if avg_w > 0.1 {
                    Some(remaining_mb / avg_w + (target_bytes as f64 / (1024.0 * 1024.0)) / (avg_w * 1.5).max(1.0))
                } else {
                    None
                };

                let emit_curve = last_curve.elapsed().as_millis() >= 150 || written_total >= target_bytes;
                if emit_curve {
                    last_curve = Instant::now();
                }

                ctx.update(|s| {
                    s.bytes_written = written_total;
                    s.current_speed_mb_s = mb_s;
                    s.avg_write_mb_s = avg_w;
                    s.eta_secs = eta;
                    s.progress = 0.5 * (written_total as f32 / target_bytes as f32);
                    if s.blocks[cell_idx].state != BlockState::Bad {
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
            Err(e) => {
                write_io_errors += 1;
                ctx.log(format!(
                    "❌ 偏移 {} 写入发生 I/O 错误: {}",
                    format_bytes(written_total),
                    e
                ));
                ctx.update(|s| {
                    s.blocks[cell_idx].state = BlockState::Bad;
                    s.blocks[cell_idx].note = Some(format!("写入 I/O 错误: {}", e));
                });
                // If disk full / disconnected, stop write phase and verify what we wrote
                break;
            }
        }
    }

    if let Some(mut f) = current_file.take() {
        let _ = f.flush_to_media();
        if local_written > 0 {
            chunk_written_sizes.push(local_written);
        }
    }

    ctx.log(format!(
        "写入阶段完成：共写入 {}（平均 {:.2} MB/s），开始全盘逐扇区回读校验...",
        format_bytes(written_total),
        ctx.state.lock().map(|s| s.avg_write_mb_s).unwrap_or(0.0)
    ));

    // Phase 2: Full unbuffered read & verify
    ctx.update(|s| {
        s.phase_label = "阶段 2/2：全盘无缓冲回读并逐扇区校验数据完整性...".to_string();
    });

    let mut rbuf = AlignedBuffer::new(IO_BLOCK);
    let mut verified_total: u64 = 0;
    let read_start = Instant::now();
    let mut last_curve = Instant::now();
    let mut first_wrap_offset: Option<u64> = None;
    let mut first_bad_offset: Option<u64> = None;
    let mut wrap_reason = String::new();
    let mut zero_or_ff_mibs: u64 = 0;

    for (c_idx, &c_len) in chunk_written_sizes.iter().enumerate() {
        let mut rf = match DirectFile::open_read(&workspace.chunk_path(c_idx)) {
            Ok(f) => f,
            Err(e) => {
                ctx.log(format!("❌ 打开分片 #{} 回读失败: {}", c_idx, e));
                continue;
            }
        };

        let mut local_read: u64 = 0;
        while local_read < c_len {
            if ctx.is_cancelled() {
                drop(rf);
                workspace.cleanup_now();
                ctx.finish(Verdict::Cancelled {
                    summary: "全盘校验已取消，所有临时文件已安全清理。".to_string(),
                });
                return;
            }

            let offset = verified_total;
            let cell_idx = ((offset / cell_bytes) as usize).min(grid_count - 1);
            let t0 = Instant::now();
            let res = rf.read_exact_aligned(&mut rbuf);
            let dt = t0.elapsed().as_secs_f64().max(1e-6);
            let mb_s = (IO_BLOCK as f64 / (1024.0 * 1024.0)) / dt;

            let (st, note) = match res {
                Ok(()) => match verify_buffer(&rbuf, run_id, offset) {
                    VerifyStatus::Ok => {
                        if mb_s < config.slow_threshold_mb_s {
                            (
                                BlockState::Slow,
                                Some(format!("校验通过，读取慢速 ({:.1} MB/s)", mb_s)),
                            )
                        } else {
                            (BlockState::Good, None)
                        }
                    }
                    VerifyStatus::Wrapped {
                        expected_offset,
                        found_offset,
                    } => {
                        if first_wrap_offset.is_none() {
                            let est = found_offset.abs_diff(expected_offset);
                            first_wrap_offset = Some(if est > 0 { est } else { offset });
                            wrap_reason = format!(
                                "在偏移 {} 处读到偏移 {} 的数据（循环覆盖扩容）",
                                format_bytes(expected_offset),
                                format_bytes(found_offset)
                            );
                            ctx.log(format!("🚨 检出扩容回绕：{}", wrap_reason));
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
                        zero_or_ff_mibs += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(offset);
                            ctx.log(format!("🚨 偏移 {} 起出现全 0x00 异常块", format_bytes(offset)));
                        }
                        (BlockState::Bad, Some("全 0x00（黑洞扩容或损坏）".to_string()))
                    }
                    VerifyStatus::FFFilled => {
                        zero_or_ff_mibs += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(offset);
                            ctx.log(format!("🚨 偏移 {} 起出现全 0xFF 异常块", format_bytes(offset)));
                        }
                        (BlockState::Bad, Some("全 0xFF（未写入或闪存损坏）".to_string()))
                    }
                    VerifyStatus::Corrupted { reason } => {
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(offset);
                        }
                        (BlockState::Bad, Some(reason))
                    }
                },
                Err(e) => {
                    if first_bad_offset.is_none() {
                        first_bad_offset = Some(offset);
                    }
                    ctx.log(format!("❌ 偏移 {} 读取失败: {}", format_bytes(offset), e));
                    (BlockState::Bad, Some(format!("读取失败: {}", e)))
                }
            };

            verified_total += IO_BLOCK as u64;
            local_read += IO_BLOCK as u64;

            let elapsed = read_start.elapsed().as_secs_f64().max(1e-6);
            let avg_r = (verified_total as f64 / (1024.0 * 1024.0)) / elapsed;
            let rem_mb = written_total.saturating_sub(verified_total) as f64 / (1024.0 * 1024.0);
            let eta = if avg_r > 0.1 { Some(rem_mb / avg_r) } else { None };
            let emit_curve = last_curve.elapsed().as_millis() >= 150 || verified_total >= written_total;
            if emit_curve {
                last_curve = Instant::now();
            }

            ctx.update(|s| {
                s.bytes_verified = verified_total;
                s.current_speed_mb_s = mb_s;
                s.avg_read_mb_s = avg_r;
                s.eta_secs = eta;
                s.progress = 0.5 + 0.5 * (verified_total as f32 / written_total.max(1) as f32);

                // Escalation priority inside a visual cell: Wrapped > Bad > Slow > Good
                let cur = s.blocks[cell_idx].state;
                let priority = |b: BlockState| -> u8 {
                    match b {
                        BlockState::Untested | BlockState::Writing | BlockState::Written => 0,
                        BlockState::Good => 1,
                        BlockState::Slow => 2,
                        BlockState::Bad => 3,
                        BlockState::Wrapped => 4,
                    }
                };
                if priority(st) >= priority(cur) {
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

    ctx.update(|s| s.phase_label = "正在自动清理临时测试文件...".to_string());
    workspace.cleanup_now();

    let (good, slow, bad, wrapped) = ctx
        .state
        .lock()
        .map(|s| s.count_blocks())
        .unwrap_or((0, 0, 0, 0));
    let reported = disk.physical_bytes.max(disk.total_bytes);

    if wrapped > 0 || (zero_or_ff_mibs >= 16 && bad >= 4) {
        let est_real = first_wrap_offset
            .or(first_bad_offset)
            .unwrap_or(0);
        let reason = if !wrap_reason.is_empty() {
            wrap_reason
        } else {
            format!(
                "全盘校验在 {} 处起持续读回全 0x00 / 0xFF，判定为扩容盘/假容量设备！",
                format_bytes(est_real)
            )
        };
        ctx.log(format!(
            "🚨 最终判定：扩容盘！标称 {}，真实有效容量约 {}",
            format_bytes_decimal(reported),
            format_bytes(est_real)
        ));
        ctx.finish(Verdict::FakeCapacity {
            reported_bytes: reported,
            estimated_real_bytes: est_real,
            reason,
        });
    } else if bad > 0 || write_io_errors > 0 {
        let summary = format!(
            "全盘校验完成（共校验 {}）：正常 {} 块，慢速 {} 块，坏块 {} 块（写入错误 {} 次）。",
            format_bytes(verified_total),
            good,
            slow,
            bad,
            write_io_errors
        );
        ctx.log(format!("⚠️ {}", summary));
        ctx.finish(Verdict::BadBlocks {
            verified_bytes: verified_total,
            bad_count: bad,
            slow_count: slow,
            summary,
        });
    } else {
        let summary = format!(
            "全盘完整性校验 100% 通过！共验证 {}，全部 {} 个区块数据零错误（慢速预警 {} 块）。",
            format_bytes(verified_total),
            good + slow,
            slow
        );
        ctx.log(format!("✅ {}", summary));
        ctx.finish(Verdict::Healthy {
            verified_bytes: verified_total,
            summary,
        });
    }
}
