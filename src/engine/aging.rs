use crate::disk::direct_io::{AlignedBuffer, DirectFile};
use crate::disk::{format_bytes, DiskInfo};
use crate::engine::pattern::{fill_buffer, generate_run_id, verify_buffer, VerifyStatus};
use crate::engine::{
    compute_target_bytes, AgingCycleRecord, EngineContext, SpeedSample, TempWorkspace, TestConfig,
    Verdict,
};
use std::time::Instant;

const IO_BLOCK: usize = 1024 * 1024; // 1 MiB

/// Burn-in / Aging Stress Test (老化 / 拷机测试).
/// Repeatedly writes fresh unique patterns and reads them back unbuffered across N cycles
/// (or continuously until stopped), tracking per-cycle error count and thermal throttling.
pub fn run(ctx: &EngineContext, disk: &DiskInfo, config: &TestConfig) {
    let base_run_id = generate_run_id();
    // Default aging footprint: 1 GiB per cycle (unless user specified a custom target size)
    let cycle_bytes = match compute_target_bytes(&disk.mount_point, config, 1024) {
        Ok(b) => b,
        Err(e) => {
            ctx.log(&e);
            ctx.finish(Verdict::Failed { error: e });
            return;
        }
    };

    let workspace = match TempWorkspace::create(&disk.mount_point, base_run_id, config.auto_cleanup)
    {
        Ok(w) => w,
        Err(e) => {
            let msg = format!("无法创建临时拷机目录: {}", e);
            ctx.log(&msg);
            ctx.finish(Verdict::Failed { error: msg });
            return;
        }
    };

    let infinite = config.aging_cycles == 0;
    let total_cycles = config.aging_cycles;
    let chunk_size = config.chunk_size_mb.clamp(64, 2048) * 1024 * 1024;

    ctx.update(|s| {
        s.target_bytes = cycle_bytes;
    });
    ctx.log(format!(
        "开始老化/拷机测试 — 单轮容量 {}，目标轮次：{}",
        format_bytes(cycle_bytes),
        if infinite {
            "无限循环（手动停止）".to_string()
        } else {
            format!("{} 轮", total_cycles)
        }
    ));

    let mut wbuf = AlignedBuffer::new(IO_BLOCK);
    let mut rbuf = AlignedBuffer::new(IO_BLOCK);
    let mut cycle = 0u32;
    let mut total_errors = 0u64;
    let mut first_cycle_write = 0.0f64;
    let mut last_cycle_write = 0.0f64;
    let mut total_bytes_written = 0u64;
    let mut total_bytes_verified = 0u64;

    loop {
        if !infinite && cycle >= total_cycles {
            break;
        }
        if ctx.is_cancelled() {
            break;
        }
        cycle += 1;
        let cycle_run_id = base_run_id.wrapping_add(cycle as u64 * 0x9E37_79B9);
        let cycle_start = Instant::now();
        let mut cycle_errors = 0u64;

        let cycle_label = if infinite {
            format!("第 {} 轮", cycle)
        } else {
            format!("第 {}/{} 轮", cycle, total_cycles)
        };

        ctx.update(|s| {
            s.phase_label = format!("老化拷机 {} — 无缓冲写入...", cycle_label);
        });

        // Write pass
        let write_start = Instant::now();
        let mut written = 0u64;
        let mut chunk_idx = 0usize;
        let mut local_written = 0u64;
        let mut chunk_sizes = Vec::new();
        let mut f_opt: Option<DirectFile> = None;

        while written < cycle_bytes {
            if ctx.is_cancelled() {
                break;
            }
            if f_opt.is_none() || local_written >= chunk_size {
                if let Some(mut f) = f_opt.take() {
                    let _ = f.flush_to_media();
                    chunk_sizes.push(local_written);
                    chunk_idx += 1;
                }
                local_written = 0;
                match DirectFile::create_rw(&workspace.chunk_path(chunk_idx)) {
                    Ok(f) => {
                        if cycle == 1 && chunk_idx == 0 {
                            let active = f.unbuffered_active;
                            ctx.update(|s| s.direct_io_active = active);
                        }
                        f_opt = Some(f);
                    }
                    Err(e) => {
                        cycle_errors += 1;
                        ctx.log(format!("❌ {} 创建分片失败: {}", cycle_label, e));
                        break;
                    }
                }
            }

            fill_buffer(&mut wbuf, cycle_run_id, written);
            let t0 = Instant::now();
            if let Err(e) = f_opt.as_mut().unwrap().write_all_aligned(&wbuf) {
                cycle_errors += 1;
                ctx.log(format!(
                    "❌ {} 写入偏移 {} 出错: {}",
                    cycle_label,
                    format_bytes(written),
                    e
                ));
                break;
            }
            let mb_s = (IO_BLOCK as f64 / (1024.0 * 1024.0)) / t0.elapsed().as_secs_f64().max(1e-6);
            written += IO_BLOCK as u64;
            local_written += IO_BLOCK as u64;
            total_bytes_written += IO_BLOCK as u64;

            let cycle_prog = (written as f32 / cycle_bytes as f32) * 0.5;
            let overall_prog = if infinite {
                cycle_prog
            } else {
                ((cycle - 1) as f32 + cycle_prog) / total_cycles as f32
            };
            ctx.update(|s| {
                s.bytes_written = total_bytes_written;
                s.current_speed_mb_s = mb_s;
                s.progress = overall_prog;
            });
        }
        if let Some(mut f) = f_opt.take() {
            let _ = f.flush_to_media();
            if local_written > 0 {
                chunk_sizes.push(local_written);
            }
        }
        if ctx.is_cancelled() {
            break;
        }
        let write_mb_s =
            (written as f64 / (1024.0 * 1024.0)) / write_start.elapsed().as_secs_f64().max(1e-6);

        // Read & Verify pass
        ctx.update(|s| {
            s.phase_label = format!("老化拷机 {} — 无缓冲回读校验...", cycle_label);
        });
        let read_start = Instant::now();
        let mut verified = 0u64;

        for (c_idx, &c_len) in chunk_sizes.iter().enumerate() {
            let mut rf = match DirectFile::open_read(&workspace.chunk_path(c_idx)) {
                Ok(f) => f,
                Err(e) => {
                    cycle_errors += 1;
                    ctx.log(format!("❌ {} 回读打开失败: {}", cycle_label, e));
                    continue;
                }
            };
            let mut local_read = 0u64;
            while local_read < c_len {
                if ctx.is_cancelled() {
                    break;
                }
                let t0 = Instant::now();
                let res = rf.read_exact_aligned(&mut rbuf);
                let mb_s =
                    (IO_BLOCK as f64 / (1024.0 * 1024.0)) / t0.elapsed().as_secs_f64().max(1e-6);
                match res {
                    Ok(()) => {
                        if verify_buffer(&rbuf, cycle_run_id, verified) != VerifyStatus::Ok {
                            cycle_errors += 1;
                        }
                    }
                    Err(_) => {
                        cycle_errors += 1;
                    }
                }
                verified += IO_BLOCK as u64;
                local_read += IO_BLOCK as u64;
                total_bytes_verified += IO_BLOCK as u64;

                let cycle_prog = 0.5 + (verified as f32 / written.max(1) as f32) * 0.5;
                let overall_prog = if infinite {
                    cycle_prog
                } else {
                    ((cycle - 1) as f32 + cycle_prog) / total_cycles as f32
                };
                ctx.update(|s| {
                    s.bytes_verified = total_bytes_verified;
                    s.current_speed_mb_s = mb_s;
                    s.progress = overall_prog;
                });
            }
            if ctx.is_cancelled() {
                break;
            }
        }
        if ctx.is_cancelled() {
            break;
        }

        let read_mb_s =
            (verified as f64 / (1024.0 * 1024.0)) / read_start.elapsed().as_secs_f64().max(1e-6);
        let duration_secs = cycle_start.elapsed().as_secs_f64();
        total_errors += cycle_errors;

        if cycle == 1 {
            first_cycle_write = write_mb_s;
        }
        last_cycle_write = write_mb_s;

        let rec = AgingCycleRecord {
            cycle,
            write_mb_s,
            read_mb_s,
            errors: cycle_errors,
            duration_secs,
        };
        ctx.log(format!(
            "{} 完成：写入 {:.2} MB/s | 读取 {:.2} MB/s | 错误 {} 次 | 耗时 {:.1}s",
            cycle_label, write_mb_s, read_mb_s, cycle_errors, duration_secs
        ));
        ctx.update(|s| {
            s.aging_records.push(rec);
            let n = s.aging_records.len() as f64;
            s.avg_write_mb_s = s.aging_records.iter().map(|r| r.write_mb_s).sum::<f64>() / n;
            s.avg_read_mb_s = s.aging_records.iter().map(|r| r.read_mb_s).sum::<f64>() / n;
            s.write_curve.push(SpeedSample {
                x: cycle as f64,
                mb_per_sec: write_mb_s,
            });
            s.read_curve.push(SpeedSample {
                x: cycle as f64,
                mb_per_sec: read_mb_s,
            });
        });
    }

    ctx.update(|s| s.phase_label = "正在自动清理拷机临时文件...".to_string());
    workspace.cleanup_now();

    let completed_cycles = ctx
        .state
        .lock()
        .map(|s| s.aging_records.len() as u32)
        .unwrap_or(0);

    if ctx.is_cancelled() && completed_cycles == 0 {
        ctx.finish(Verdict::Cancelled {
            summary: "老化拷机测试已取消。".to_string(),
        });
        return;
    }

    let throttle_pct = if first_cycle_write > 0.1 && completed_cycles >= 2 {
        ((first_cycle_write - last_cycle_write) / first_cycle_write * 100.0).max(0.0)
    } else {
        0.0
    };

    let summary = if total_errors == 0 {
        format!(
            "老化拷机通过：共完成 {} 轮（总写入 {}），零校验错误，连续写入掉速幅度 {:.1}%。",
            completed_cycles,
            format_bytes(total_bytes_written),
            throttle_pct
        )
    } else {
        format!(
            "老化拷机发现异常：共完成 {} 轮，累计错误 {} 次，连续写入掉速幅度 {:.1}%，不建议存放重要数据！",
            completed_cycles, total_errors, throttle_pct
        )
    };
    ctx.log(&summary);
    ctx.finish(Verdict::AgingComplete {
        cycles_done: completed_cycles,
        total_errors,
        throttle_pct,
        summary,
    });
}
