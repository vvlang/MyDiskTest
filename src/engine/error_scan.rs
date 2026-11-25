use crate::disk::direct_io::{AlignedBuffer, DirectFile};
use crate::disk::{format_bytes, format_bytes_decimal, DiskInfo};
use crate::engine::pattern::generate_run_id;
use crate::engine::{
    BadSectorEntry, BlockCell, BlockState, EngineContext, SpeedSample, TestConfig, Verdict,
};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;
const FULL_IO_SIZE: usize = 2 * 1024 * 1024; // 2 MiB aligned sequential surface read
const QUICK_IO_SIZE: usize = 1024 * 1024; // 1 MiB sample per block in Quick Scan
const DRILL_IO_SIZE: usize = 64 * 1024; // 64 KiB drill-down when a bad sector is hit

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ScanEvent {
    Init {
        scan_bytes: u64,
        start_offset: u64,
        grid_count: usize,
        cell_bytes: u64,
    },
    Phase {
        label: String,
    },
    Log {
        msg: String,
    },
    CellUpdate {
        index: usize,
        state: BlockState,
        read_mb_s: f32,
        progress: f32,
        bytes_scanned: u64,
        current_mb_s: f64,
        avg_read_mb_s: f64,
        eta_secs: Option<f64>,
        curve_pt: Option<SpeedSample>,
        note: Option<String>,
        bad_entry: Option<BadSectorEntry>,
    },
    Done {
        verdict: Verdict,
    },
}

/// Entrypoint for HD Tune Pro style Error Scan (100% Read-Only Surface Bad-Sector Scanner).
pub fn run(ctx: &EngineContext, disk: &DiskInfo, config: &TestConfig) {
    if let Some(ref raw_path) = disk.raw_disk_path {
        run_raw_disk_error_scan(ctx, disk, raw_path, config);
    } else {
        run_folder_read_scan(ctx, disk, config);
    }
}

fn compute_scan_bounds(disk_bytes: u64, config: &TestConfig) -> Result<(u64, u64), String> {
    if disk_bytes < 4 * MIB {
        return Err("目标磁盘容量过小，无法执行错误扫描。".to_string());
    }
    let start_raw = (config.error_scan_start_gb.max(0.0) * GIB as f64) as u64;
    let start = (start_raw / 4096) * 4096;
    let end_raw = if config.error_scan_end_gb > 0.0 {
        ((config.error_scan_end_gb * GIB as f64) as u64).min(disk_bytes)
    } else {
        disk_bytes
    };
    let end = (end_raw / 4096) * 4096;
    if end <= start + 4 * MIB {
        return Err("扫描起止区间无效：结束位置必须大于起始位置（至少 4 MiB）。".to_string());
    }
    Ok((start, end))
}

fn run_raw_disk_error_scan(
    ctx: &EngineContext,
    disk: &DiskInfo,
    raw_path: &str,
    config: &TestConfig,
) {
    let disk_bytes = disk.physical_bytes.max(disk.total_bytes);
    let (start_offset, end_offset) = match compute_scan_bounds(disk_bytes, config) {
        Ok(b) => b,
        Err(e) => {
            ctx.log(&e);
            ctx.finish(Verdict::Failed { error: e });
            return;
        }
    };
    let grid_count = config.grid_blocks.clamp(100, 2500);

    // First try opening raw device directly without elevation (works if user already ran with sudo/Admin).
    if let Ok(mut dev) = DirectFile::open_read(Path::new(raw_path)) {
        ctx.update(|s| s.direct_io_active = dev.unbuffered_active);
        ctx.log(format!(
            "已直接打开底层只读设备 {}，开始 HD Tune Pro 错误扫描（{}）",
            raw_path,
            if config.error_scan_quick {
                "快速扫描"
            } else {
                "完整表面扫描"
            }
        ));
        let emit = |ev: &ScanEvent| {
            let _ = apply_scan_event(ctx, ev.clone());
        };
        execute_surface_scan(
            &mut dev,
            raw_path,
            start_offset,
            end_offset,
            grid_count,
            config.error_scan_quick,
            config.slow_threshold_mb_s,
            || ctx.is_cancelled(),
            &emit,
        );
        return;
    }

    // Otherwise elevate via `--error-scan-worker` (100% O_RDONLY — NO unmount needed!).
    let run_id = generate_run_id();
    let status_file =
        std::env::temp_dir().join(format!("mdt_escan_{:08x}.jsonl", run_id & 0xFFFF_FFFF));
    let cancel_file =
        std::env::temp_dir().join(format!("mdt_escan_{:08x}.cancel", run_id & 0xFFFF_FFFF));
    let _ = std::fs::remove_file(&status_file);
    let _ = std::fs::remove_file(&cancel_file);
    if let Err(e) = File::create(&status_file) {
        ctx.finish(Verdict::Failed {
            error: format!("无法创建错误扫描通信文件: {}", e),
        });
        return;
    }

    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            ctx.finish(Verdict::Failed {
                error: format!("无法定位当前程序路径: {}", e),
            });
            return;
        }
    };

    ctx.update(|s| {
        s.direct_io_active = true;
        s.target_bytes = end_offset - start_offset;
        s.phase_label = "正在请求管理员权限以只读访问底层物理扇区（零写入、无需卸载）...".to_string();
    });
    ctx.log(format!(
        "准备启动 HD Tune Pro 只读错误扫描：设备 {}，区间 {} ~ {}，模式：{}，网格 {} 块",
        raw_path,
        format_bytes(start_offset),
        format_bytes(end_offset),
        if config.error_scan_quick {
            "快速扫描"
        } else {
            "完整扫描"
        },
        grid_count
    ));

    let mut child = match spawn_elevated_scan_worker(
        &exe_path,
        raw_path,
        start_offset,
        end_offset,
        grid_count,
        config.error_scan_quick,
        config.slow_threshold_mb_s,
        &status_file,
        &cancel_file,
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&status_file);
            ctx.finish(Verdict::Failed { error: e });
            return;
        }
    };

    let mut lines_read = 0usize;
    let mut got_done = false;
    let mut sent_cancel = false;

    loop {
        if ctx.is_cancelled() && !sent_cancel {
            sent_cancel = true;
            let _ = File::create(&cancel_file);
            ctx.log("正在停止错误扫描...");
        }

        if let Ok(file) = File::open(&status_file) {
            let reader = BufReader::new(file);
            for (idx, line_res) in reader.lines().enumerate() {
                if idx < lines_read {
                    continue;
                }
                if let Ok(line) = line_res {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    lines_read = idx + 1;
                    if let Ok(ev) = serde_json::from_str::<ScanEvent>(trimmed) {
                        if apply_scan_event(ctx, ev) {
                            got_done = true;
                        }
                    }
                }
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                if let Ok(file) = File::open(&status_file) {
                    let reader = BufReader::new(file);
                    for (idx, line_res) in reader.lines().enumerate() {
                        if idx < lines_read {
                            continue;
                        }
                        if let Ok(line) = line_res {
                            if let Ok(ev) = serde_json::from_str::<ScanEvent>(line.trim()) {
                                if apply_scan_event(ctx, ev) {
                                    got_done = true;
                                }
                            }
                        }
                    }
                }
                if !got_done {
                    if ctx.is_cancelled() {
                        ctx.finish(Verdict::Cancelled {
                            summary: "错误扫描已手动停止。".to_string(),
                        });
                    } else if !status.success() {
                        ctx.finish(Verdict::Failed {
                            error: "管理员授权已取消或底层只读扫描未能启动。".to_string(),
                        });
                    } else {
                        ctx.finish(Verdict::Failed {
                            error: "错误扫描进程提前退出。".to_string(),
                        });
                    }
                }
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(80)),
            Err(e) => {
                ctx.finish(Verdict::Failed {
                    error: format!("等待错误扫描进程失败: {}", e),
                });
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&status_file);
    let _ = std::fs::remove_file(&cancel_file);
}

fn apply_scan_event(ctx: &EngineContext, ev: ScanEvent) -> bool {
    match ev {
        ScanEvent::Init {
            scan_bytes,
            start_offset,
            grid_count,
            cell_bytes,
        } => {
            let cells: Vec<BlockCell> = (0..grid_count)
                .map(|i| {
                    let off = start_offset + (i as u64) * cell_bytes;
                    let sz = cell_bytes.min((start_offset + scan_bytes).saturating_sub(off));
                    BlockCell {
                        index: i,
                        state: BlockState::Untested,
                        offset_bytes: off,
                        size_bytes: sz,
                        write_mb_s: 0.0,
                        read_mb_s: 0.0,
                        note: None,
                    }
                })
                .collect();
            ctx.update(|s| {
                s.target_bytes = scan_bytes;
                s.blocks = cells;
            });
            false
        }
        ScanEvent::Phase { label } => {
            ctx.update(|s| s.phase_label = label);
            false
        }
        ScanEvent::Log { msg } => {
            ctx.log(msg);
            false
        }
        ScanEvent::CellUpdate {
            index,
            state,
            read_mb_s,
            progress,
            bytes_scanned,
            current_mb_s,
            avg_read_mb_s,
            eta_secs,
            curve_pt,
            note,
            bad_entry,
        } => {
            ctx.update(|s| {
                s.progress = progress;
                s.bytes_verified = bytes_scanned;
                s.current_speed_mb_s = current_mb_s;
                s.avg_read_mb_s = avg_read_mb_s;
                s.eta_secs = eta_secs;
                if let Some(cell) = s.blocks.get_mut(index) {
                    cell.state = state;
                    cell.read_mb_s = read_mb_s;
                    if note.is_some() {
                        cell.note = note;
                    }
                }
                if let Some(pt) = curve_pt {
                    s.read_curve.push(pt);
                }
                if let Some(entry) = bad_entry {
                    if s.bad_sectors.len() < 5000 {
                        s.bad_sectors.push(entry);
                    }
                }
            });
            false
        }
        ScanEvent::Done { verdict } => {
            ctx.finish(verdict);
            true
        }
    }
}

/// Core HD Tune Pro surface scanner (works on raw `/dev/rdiskN`, `\\.\PhysicalDriveN`, or test image files).
#[allow(clippy::too_many_arguments)]
fn execute_surface_scan<C, E>(
    dev: &mut DirectFile,
    dev_label: &str,
    start_offset: u64,
    end_offset: u64,
    grid_count: usize,
    quick_scan: bool,
    slow_threshold_mb_s: f64,
    is_cancelled: C,
    emit: &E,
) where
    C: Fn() -> bool,
    E: Fn(&ScanEvent),
{
    let scan_bytes = end_offset.saturating_sub(start_offset);
    let actual_grid = grid_count.clamp(16, 2500);
    let cell_bytes = (scan_bytes / actual_grid as u64).max(4096);

    emit(&ScanEvent::Init {
        scan_bytes,
        start_offset,
        grid_count: actual_grid,
        cell_bytes,
    });
    emit(&ScanEvent::Phase {
        label: format!(
            "HD Tune Pro 错误扫描（{}，只读零写入）— 每格约 {}",
            if quick_scan { "快速扫描" } else { "完整表面扫描" },
            format_bytes(cell_bytes)
        ),
    });
    emit(&ScanEvent::Log {
        msg: format!(
            "开始扫描 {}：范围 {} ~ {}（共 {}），{} 格，每格 {}",
            dev_label,
            format_bytes(start_offset),
            format_bytes(end_offset),
            format_bytes_decimal(scan_bytes),
            actual_grid,
            format_bytes(cell_bytes)
        ),
    });

    let scan_start = Instant::now();
    let mut total_bytes_read: u64 = 0;
    let mut good_blocks = 0usize;
    let mut slow_blocks = 0usize;
    let mut bad_blocks = 0usize;
    let mut last_curve_time = Instant::now();

    if quick_scan {
        // ---- HD Tune Pro Quick Scan: read aligned 1 MiB sample at every grid cell ----
        let sample_len = (QUICK_IO_SIZE as u64).min(cell_bytes);
        let aligned_sample = ((sample_len / 4096).max(1) * 4096) as usize;
        let mut rbuf = AlignedBuffer::new(aligned_sample);

        for cell_idx in 0..actual_grid {
            if is_cancelled() {
                emit(&ScanEvent::Done {
                    verdict: Verdict::Cancelled {
                        summary: format!(
                            "快速错误扫描已停止：已扫 {}/{} 格（正常 {} / 慢速 {} / 坏道 {}）。",
                            cell_idx, actual_grid, good_blocks, slow_blocks, bad_blocks
                        ),
                    },
                });
                return;
            }
            let off = ((start_offset + (cell_idx as u64) * cell_bytes) / 4096) * 4096;
            if off + (aligned_sample as u64) > end_offset {
                break;
            }

            let t0 = Instant::now();
            let res = dev.read_at(off, &mut rbuf);
            let dt = t0.elapsed().as_secs_f64().max(1e-6);
            let mb_s = (aligned_sample as f64 / (1024.0 * 1024.0)) / dt;

            let (state, note, bad_entry) = match res {
                Ok(()) => {
                    total_bytes_read += aligned_sample as u64;
                    if mb_s < slow_threshold_mb_s {
                        slow_blocks += 1;
                        let reason = format!("读取偏慢 ({:.2} MB/s，低于阈值 {:.1} MB/s)", mb_s, slow_threshold_mb_s);
                        let entry = BadSectorEntry {
                            block_index: cell_idx,
                            offset_bytes: off,
                            lba_512: off / 512,
                            speed_mb_s: mb_s,
                            is_hard_error: false,
                            reason: reason.clone(),
                        };
                        (BlockState::Slow, Some(reason), Some(entry))
                    } else {
                        good_blocks += 1;
                        (BlockState::Good, None, None)
                    }
                }
                Err(e) => {
                    bad_blocks += 1;
                    let reason = format!("扇区读取失败 (I/O 错误): {}", e);
                    emit(&ScanEvent::Log {
                        msg: format!(
                            "❌ 发现坏道！区块 #{} | 偏移 {} (LBA {}) — {}",
                            cell_idx + 1,
                            format_bytes(off),
                            off / 512,
                            e
                        ),
                    });
                    let entry = BadSectorEntry {
                        block_index: cell_idx,
                        offset_bytes: off,
                        lba_512: off / 512,
                        speed_mb_s: 0.0,
                        is_hard_error: true,
                        reason: reason.clone(),
                    };
                    (BlockState::Bad, Some(reason), Some(entry))
                }
            };

            let elapsed = scan_start.elapsed().as_secs_f64().max(1e-6);
            let avg_r = (total_bytes_read as f64 / (1024.0 * 1024.0)) / elapsed;
            let prog = (cell_idx + 1) as f32 / actual_grid as f32;
            let eta = if prog > 0.01 {
                Some(elapsed * ((1.0 - prog as f64) / prog as f64))
            } else {
                None
            };
            let emit_curve = last_curve_time.elapsed().as_millis() >= 60 || cell_idx + 1 == actual_grid;
            if emit_curve {
                last_curve_time = Instant::now();
            }

            emit(&ScanEvent::CellUpdate {
                index: cell_idx,
                state,
                read_mb_s: mb_s as f32,
                progress: prog,
                bytes_scanned: total_bytes_read,
                current_mb_s: mb_s,
                avg_read_mb_s: avg_r,
                eta_secs: eta,
                curve_pt: if emit_curve {
                    Some(SpeedSample {
                        x: off as f64 / GIB as f64,
                        mb_per_sec: mb_s,
                    })
                } else {
                    None
                },
                note,
                bad_entry,
            });
        }
    } else {
        // ---- HD Tune Pro Standard Full Surface Scan: read 100% of every byte in range ----
        let mut rbuf = AlignedBuffer::new(FULL_IO_SIZE);
        let mut drill_buf = AlignedBuffer::new(DRILL_IO_SIZE);

        for cell_idx in 0..actual_grid {
            let cell_start = ((start_offset + (cell_idx as u64) * cell_bytes) / 4096) * 4096;
            let cell_end = if cell_idx + 1 == actual_grid {
                end_offset
            } else {
                ((start_offset + ((cell_idx + 1) as u64) * cell_bytes) / 4096) * 4096
            }
            .min(end_offset);

            if cell_start >= cell_end {
                continue;
            }

            let cell_t0 = Instant::now();
            let mut pos = cell_start;
            let mut cell_read: u64 = 0;
            let mut cell_hard_errors: usize = 0;
            let mut first_err_offset: Option<u64> = None;
            let mut first_err_msg = String::new();

            while pos < cell_end {
                if is_cancelled() {
                    emit(&ScanEvent::Done {
                        verdict: Verdict::Cancelled {
                            summary: format!(
                                "完整错误扫描已停止：已扫 {}/{} 格（正常 {} / 慢速 {} / 坏道 {}）。",
                                cell_idx, actual_grid, good_blocks, slow_blocks, bad_blocks
                            ),
                        },
                    });
                    return;
                }

                let rem = cell_end - pos;
                let step = (FULL_IO_SIZE as u64).min(rem);
                let aligned_step = ((step / 4096) * 4096) as usize;
                if aligned_step == 0 {
                    break;
                }

                let res = dev.read_at(pos, &mut rbuf.as_mut_slice()[..aligned_step]);
                match res {
                    Ok(()) => {
                        cell_read += aligned_step as u64;
                        total_bytes_read += aligned_step as u64;
                    }
                    Err(_) => {
                        // Drill down at 64 KiB granularity to pinpoint exact bad LBA!
                        let mut dpos = pos;
                        let dend = pos + aligned_step as u64;
                        while dpos < dend {
                            let dstep = (DRILL_IO_SIZE as u64).min(dend - dpos);
                            let daligned = ((dstep / 4096) * 4096) as usize;
                            if daligned == 0 {
                                break;
                            }
                            if let Err(de) = dev.read_at(dpos, &mut drill_buf.as_mut_slice()[..daligned]) {
                                cell_hard_errors += 1;
                                if first_err_offset.is_none() {
                                    first_err_offset = Some(dpos);
                                    first_err_msg = de.to_string();
                                }
                            } else {
                                cell_read += daligned as u64;
                                total_bytes_read += daligned as u64;
                            }
                            dpos += daligned as u64;
                        }
                    }
                }
                pos += aligned_step as u64;
            }

            let cell_dt = cell_t0.elapsed().as_secs_f64().max(1e-6);
            let cell_mb_s = (cell_read as f64 / (1024.0 * 1024.0)) / cell_dt;
            let (state, note, bad_entry) = if cell_hard_errors > 0 {
                bad_blocks += 1;
                let bad_off = first_err_offset.unwrap_or(cell_start);
                let reason = format!(
                    "读取坏道 ({} 处错误，首个错误 LBA {}: {})",
                    cell_hard_errors,
                    bad_off / 512,
                    first_err_msg
                );
                emit(&ScanEvent::Log {
                    msg: format!(
                        "❌ 发现坏道！区块 #{} | 偏移 {} (LBA {}) — {}",
                        cell_idx + 1,
                        format_bytes(bad_off),
                        bad_off / 512,
                        first_err_msg
                    ),
                });
                let entry = BadSectorEntry {
                    block_index: cell_idx,
                    offset_bytes: bad_off,
                    lba_512: bad_off / 512,
                    speed_mb_s: cell_mb_s,
                    is_hard_error: true,
                    reason: reason.clone(),
                };
                (BlockState::Bad, Some(reason), Some(entry))
            } else if cell_mb_s < slow_threshold_mb_s {
                slow_blocks += 1;
                let reason = format!(
                    "读取慢速预警 ({:.2} MB/s，低于 {:.1} MB/s)",
                    cell_mb_s, slow_threshold_mb_s
                );
                let entry = BadSectorEntry {
                    block_index: cell_idx,
                    offset_bytes: cell_start,
                    lba_512: cell_start / 512,
                    speed_mb_s: cell_mb_s,
                    is_hard_error: false,
                    reason: reason.clone(),
                };
                (BlockState::Slow, Some(reason), Some(entry))
            } else {
                good_blocks += 1;
                (BlockState::Good, None, None)
            };

            let elapsed = scan_start.elapsed().as_secs_f64().max(1e-6);
            let avg_r = (total_bytes_read as f64 / (1024.0 * 1024.0)) / elapsed;
            let prog = (cell_idx + 1) as f32 / actual_grid as f32;
            let rem_mb = scan_bytes.saturating_sub(total_bytes_read) as f64 / (1024.0 * 1024.0);
            let eta = if avg_r > 0.1 { Some(rem_mb / avg_r) } else { None };
            let emit_curve = last_curve_time.elapsed().as_millis() >= 100 || cell_idx + 1 == actual_grid;
            if emit_curve {
                last_curve_time = Instant::now();
            }

            emit(&ScanEvent::CellUpdate {
                index: cell_idx,
                state,
                read_mb_s: cell_mb_s as f32,
                progress: prog,
                bytes_scanned: total_bytes_read,
                current_mb_s: cell_mb_s,
                avg_read_mb_s: avg_r,
                eta_secs: eta,
                curve_pt: if emit_curve {
                    Some(SpeedSample {
                        x: cell_start as f64 / GIB as f64,
                        mb_per_sec: cell_mb_s,
                    })
                } else {
                    None
                },
                note,
                bad_entry,
            });
        }
    }

    let total_checked = (good_blocks + slow_blocks + bad_blocks).max(1);
    let damaged_pct = (bad_blocks as f64 / total_checked as f64) * 100.0;
    let verdict = if bad_blocks > 0 {
        Verdict::BadBlocks {
            verified_bytes: total_bytes_read,
            bad_count: bad_blocks,
            slow_count: slow_blocks,
            summary: format!(
                "HD Tune Pro 错误扫描发现坏道！共扫描 {} 格（实读 {}）：损坏 {} 格 ({:.2}%)，慢速预警 {} 格，正常 {} 格。请立即备份重要数据！",
                total_checked,
                format_bytes(total_bytes_read),
                bad_blocks,
                damaged_pct,
                slow_blocks,
                good_blocks
            ),
        }
    } else if slow_blocks > 0 {
        Verdict::Healthy {
            verified_bytes: total_bytes_read,
            summary: format!(
                "错误扫描完成（无硬坏道，损坏率 0.0%）：共扫描 {} 格（实读 {}），发现 {} 格读取偏慢预警，其余 {} 格完全健康。",
                total_checked,
                format_bytes(total_bytes_read),
                slow_blocks,
                good_blocks
            ),
        }
    } else {
        Verdict::Healthy {
            verified_bytes: total_bytes_read,
            summary: format!(
                "HD Tune Pro 错误扫描 100% 健康通过！损坏率 0.0%（共扫描 {} 格，覆盖范围 {}，零坏道、零慢速块）。",
                total_checked,
                format_bytes_decimal(scan_bytes)
            ),
        }
    };

    emit(&ScanEvent::Log {
        msg: format!(
            "错误扫描结束：正常 {} | 慢速 {} | 坏道 {}（损坏率 {:.2}%）",
            good_blocks, slow_blocks, bad_blocks, damaged_pct
        ),
    });
    emit(&ScanEvent::Done { verdict });
}

/// Fallback read-only scanner when user chooses a custom directory instead of a physical disk:
/// recursively scans all existing files in the folder via unbuffered read to find unreadable files/blocks.
fn run_folder_read_scan(ctx: &EngineContext, disk: &DiskInfo, config: &TestConfig) {
    ctx.update(|s| {
        s.phase_label = "正在枚举目标目录内全部文件进行只读可读性扫描...".to_string();
    });

    let mut files = Vec::new();
    let mut total_size: u64 = 0;
    collect_files_recursive(&disk.mount_point, &mut files, &mut total_size);

    if files.is_empty() || total_size == 0 {
        let msg = "该自定义目录中没有任何可供只读扫描的文件（如需全盘表面错误扫描，请在左侧选择具体物理磁盘）。".to_string();
        ctx.log(&msg);
        ctx.finish(Verdict::Failed { error: msg });
        return;
    }

    let grid_count = files.len().min(config.grid_blocks.clamp(64, 1000));
    let cells: Vec<BlockCell> = (0..grid_count)
        .map(|i| BlockCell {
            index: i,
            state: BlockState::Untested,
            offset_bytes: (total_size / grid_count as u64) * i as u64,
            size_bytes: (total_size / grid_count as u64).max(1),
            write_mb_s: 0.0,
            read_mb_s: 0.0,
            note: None,
        })
        .collect();

    ctx.update(|s| {
        s.target_bytes = total_size;
        s.blocks = cells;
        s.phase_label = format!("只读文件错误扫描（共 {} 个文件，{}）...", files.len(), format_bytes(total_size));
    });

    let mut buf = vec![0u8; 1024 * 1024];
    let start = Instant::now();
    let mut read_total: u64 = 0;
    let mut bad_files = 0usize;

    for (idx, (path, fsize)) in files.iter().enumerate() {
        if ctx.is_cancelled() {
            ctx.finish(Verdict::Cancelled {
                summary: "目录只读错误扫描已停止。".to_string(),
            });
            return;
        }
        let cell_idx = (idx * grid_count / files.len()).min(grid_count - 1);
        let t0 = Instant::now();
        let mut ok = true;
        let mut err_msg = String::new();

        match File::open(path) {
            Ok(mut f) => {
                use std::io::Read;
                loop {
                    match f.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => read_total += n as u64,
                        Err(e) => {
                            ok = false;
                            err_msg = format!("{} 读取失败: {}", path.display(), e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                ok = false;
                err_msg = format!("{} 打开失败: {}", path.display(), e);
            }
        }

        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (*fsize as f64 / (1024.0 * 1024.0)) / dt;
        let avg_r = (read_total as f64 / (1024.0 * 1024.0)) / start.elapsed().as_secs_f64().max(1e-6);

        if !ok {
            bad_files += 1;
            ctx.log(format!("❌ {}", err_msg));
        }
        ctx.update(|s| {
            s.bytes_verified = read_total;
            s.current_speed_mb_s = mb_s;
            s.avg_read_mb_s = avg_r;
            s.progress = (idx + 1) as f32 / files.len() as f32;
            if !ok {
                s.blocks[cell_idx].state = BlockState::Bad;
                s.blocks[cell_idx].note = Some(err_msg.clone());
                s.bad_sectors.push(BadSectorEntry {
                    block_index: cell_idx,
                    offset_bytes: read_total,
                    lba_512: 0,
                    speed_mb_s: 0.0,
                    is_hard_error: true,
                    reason: err_msg.clone(),
                });
            } else if s.blocks[cell_idx].state != BlockState::Bad {
                s.blocks[cell_idx].state = BlockState::Good;
                s.blocks[cell_idx].read_mb_s = mb_s as f32;
            }
        });
    }

    if bad_files > 0 {
        ctx.finish(Verdict::BadBlocks {
            verified_bytes: read_total,
            bad_count: bad_files,
            slow_count: 0,
            summary: format!("目录只读扫描完成：发现 {} 个不可读/损坏文件。", bad_files),
        });
    } else {
        ctx.finish(Verdict::Healthy {
            verified_bytes: read_total,
            summary: format!(
                "目录只读扫描通过：全部 {} 个文件（{}）读取正常，零损坏。",
                files.len(),
                format_bytes(read_total)
            ),
        });
    }
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<(PathBuf, u64)>, total: &mut u64) {
    if out.len() >= 10000 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_files_recursive(&p, out, total);
            } else if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    let len = meta.len();
                    *total += len;
                    out.push((p, len));
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn spawn_elevated_scan_worker(
    exe_path: &Path,
    raw_path: &str,
    start_offset: u64,
    end_offset: u64,
    grid_count: usize,
    quick: bool,
    slow_threshold: f64,
    status_file: &Path,
    cancel_file: &Path,
) -> Result<std::process::Child, String> {
    let sh_quote = |s: &str| -> String { format!("'{}'", s.replace('\'', "'\\''")) };
    let inner_cmd = format!(
        "{} --error-scan-worker {} {} {} {} {} {:.2} {} {}",
        sh_quote(&exe_path.to_string_lossy()),
        sh_quote(raw_path),
        start_offset,
        end_offset,
        grid_count,
        if quick { "1" } else { "0" },
        slow_threshold,
        sh_quote(&status_file.to_string_lossy()),
        sh_quote(&cancel_file.to_string_lossy()),
    );
    let escaped = inner_cmd.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"MyDiskTest 需要管理员权限以只读方式扫描底层物理扇区（零写入、不会改动磁盘数据）：\"",
        escaped
    );
    Command::new("osascript")
        .args(["-e", &script])
        .spawn()
        .map_err(|e| format!("无法调用 macOS 授权弹窗: {}", e))
}

#[cfg(target_os = "windows")]
#[allow(clippy::too_many_arguments)]
fn spawn_elevated_scan_worker(
    exe_path: &Path,
    raw_path: &str,
    start_offset: u64,
    end_offset: u64,
    grid_count: usize,
    quick: bool,
    slow_threshold: f64,
    status_file: &Path,
    cancel_file: &Path,
) -> Result<std::process::Child, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let args_str = format!(
        "--error-scan-worker \"{}\" {} {} {} {} {:.2} \"{}\" \"{}\"",
        raw_path,
        start_offset,
        end_offset,
        grid_count,
        if quick { "1" } else { "0" },
        slow_threshold,
        status_file.display(),
        cancel_file.display()
    );
    let ps = format!(
        "Start-Process -FilePath '{}' -ArgumentList '{}' -Verb RunAs -Wait -WindowStyle Hidden",
        exe_path.display().to_string().replace('\'', "''"),
        args_str.replace('\'', "''")
    );
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("无法触发 Windows UAC 管理员授权: {}", e))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[allow(clippy::too_many_arguments)]
fn spawn_elevated_scan_worker(
    exe_path: &Path,
    raw_path: &str,
    start_offset: u64,
    end_offset: u64,
    grid_count: usize,
    quick: bool,
    slow_threshold: f64,
    status_file: &Path,
    cancel_file: &Path,
) -> Result<std::process::Child, String> {
    Command::new(exe_path)
        .args([
            "--error-scan-worker",
            raw_path,
            &start_offset.to_string(),
            &end_offset.to_string(),
            &grid_count.to_string(),
            if quick { "1" } else { "0" },
            &format!("{:.2}", slow_threshold),
            &status_file.to_string_lossy(),
            &cancel_file.to_string_lossy(),
        ])
        .spawn()
        .map_err(|e| format!("启动错误扫描进程失败: {}", e))
}

/// Headless elevated worker entrypoint for `--error-scan-worker`.
pub fn run_cli_worker(args: &[String]) -> i32 {
    if args.len() < 8 {
        eprintln!("Invalid --error-scan-worker arguments");
        return 2;
    }
    let raw_path = &args[0];
    let start_offset: u64 = args[1].parse().unwrap_or(0);
    let end_offset: u64 = args[2].parse().unwrap_or(0);
    let grid_count: usize = args[3].parse().unwrap_or(1000).clamp(100, 2500);
    let quick = args[4] == "1";
    let slow_threshold: f64 = args[5].parse().unwrap_or(5.0);
    let status_path = PathBuf::from(&args[6]);
    let cancel_path = PathBuf::from(&args[7]);

    let emit = |ev: &ScanEvent| {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&status_path) {
            if let Ok(line) = serde_json::to_string(ev) {
                let _ = writeln!(f, "{}", line);
                let _ = f.flush();
            }
        }
    };

    let mut dev = match DirectFile::open_read(Path::new(raw_path)) {
        Ok(f) => f,
        Err(e) => {
            #[cfg(target_os = "macos")]
            let msg = if e.raw_os_error() == Some(1) {
                format!(
                    "macOS 隐私保护拦截了底层只读设备 {} 访问（Operation not permitted）。\n\
                    请点击下方「打开完全磁盘访问权限设置」按钮，在「系统设置 → 隐私与安全性 → 完全磁盘访问权限」中添加并开启 MyDiskTest.app，重启本软件后即可进行全盘表面错误扫描！",
                    raw_path
                )
            } else {
                format!("无法只读打开物理设备 {}: {}", raw_path, e)
            };
            #[cfg(not(target_os = "macos"))]
            let msg = format!("无法只读打开物理设备 {}: {}", raw_path, e);

            emit(&ScanEvent::Done {
                verdict: Verdict::Failed { error: msg },
            });
            return 1;
        }
    };

    execute_surface_scan(
        &mut dev,
        raw_path,
        start_offset,
        end_offset,
        grid_count,
        quick,
        slow_threshold,
        || cancel_path.exists(),
        &emit,
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_scan_quick_and_full_on_mock_image() {
        let dir = std::env::temp_dir().join(format!("mdt_escan_unit_{:x}", generate_run_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("disk.img");
        let size = 16 * MIB;
        {
            let f = File::create(&img).unwrap();
            f.set_len(size).unwrap();
        }

        // Test Quick Scan
        let mut dev = DirectFile::open_read(&img).unwrap();
        let done_verdict = std::cell::RefCell::new(None);
        let emit = |ev: &ScanEvent| {
            if let ScanEvent::Done { verdict } = ev {
                *done_verdict.borrow_mut() = Some(verdict.clone());
            }
        };
        execute_surface_scan(&mut dev, "mock_quick", 0, size, 16, true, 0.01, || false, &emit);
        assert!(matches!(&*done_verdict.borrow(), Some(Verdict::Healthy { .. })));

        // Test Full Scan
        let done_verdict_full = std::cell::RefCell::new(None);
        let emit_full = |ev: &ScanEvent| {
            if let ScanEvent::Done { verdict } = ev {
                *done_verdict_full.borrow_mut() = Some(verdict.clone());
            }
        };
        execute_surface_scan(&mut dev, "mock_full", 0, size, 16, false, 0.01, || false, &emit_full);
        assert!(matches!(&*done_verdict_full.borrow(), Some(Verdict::Healthy { .. })));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
