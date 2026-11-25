use crate::disk::direct_io::{AlignedBuffer, DirectFile};
use crate::disk::{format_bytes, DiskInfo};
use crate::engine::pattern::{fill_buffer, generate_run_id};
use crate::engine::{
    compute_target_bytes, BenchmarkMetrics, EngineContext, SpeedSample, TempWorkspace, TestConfig,
    Verdict,
};
use std::time::Instant;

const MIB: u64 = 1024 * 1024;
const SEQ_BLOCK: usize = 1024 * 1024; // 1 MiB
const MED_BLOCK: usize = 64 * 1024; // 64 KiB
const RAND_BLOCK: usize = 4096; // 4 KiB

/// Runs a comprehensive Read/Write Speed Benchmark:
/// 1. Sequential 1 MiB Write (with real-time speed curve to expose SLC cache drop-off)
/// 2. Sequential 1 MiB Read (unbuffered Direct I/O, with speed curve)
/// 3. 64 KiB Medium-Block Write & Read
/// 4. 4 KiB Random Write & Read (QD1: MB/s, IOPS, average latency in ms)
pub fn run(ctx: &EngineContext, disk: &DiskInfo, config: &TestConfig) {
    let run_id = generate_run_id();
    // Default benchmark test size: 1 GiB (or user-specified size, clamped to free space)
    let target_bytes = match compute_target_bytes(&disk.mount_point, config, 1024) {
        Ok(b) => b,
        Err(e) => {
            ctx.log(&e);
            ctx.finish(Verdict::Failed { error: e });
            return;
        }
    };

    let workspace = match TempWorkspace::create(&disk.mount_point, run_id, config.auto_cleanup) {
        Ok(w) => w,
        Err(e) => {
            let msg = format!("无法创建临时测试目录: {}", e);
            ctx.log(&msg);
            ctx.finish(Verdict::Failed { error: msg });
            return;
        }
    };

    ctx.update(|s| {
        s.target_bytes = target_bytes;
        s.phase_label = "阶段 1/6：顺序 1MB 写入速度测试...".to_string();
    });
    ctx.log(format!(
        "开始读写速度基准测试 — 顺序测试文件大小 {}",
        format_bytes(target_bytes)
    ));

    let seq_path = workspace.dir.join("bench_seq.bin");
    let mut metrics = BenchmarkMetrics::default();

    // ---- 1. Sequential 1 MiB Write ----
    let mut buf1m = AlignedBuffer::new(SEQ_BLOCK);
    fill_buffer(&mut buf1m, run_id, 0);

    let mut f = match DirectFile::create_rw(&seq_path) {
        Ok(f) => f,
        Err(e) => {
            ctx.finish(Verdict::Failed {
                error: format!("创建测速文件失败: {}", e),
            });
            return;
        }
    };
    ctx.update(|s| s.direct_io_active = f.unbuffered_active);

    let mut written = 0u64;
    let mut min_w = f64::MAX;
    let mut max_w = 0.0f64;
    let t_start = Instant::now();
    let mut last_sample = Instant::now();

    while written < target_bytes {
        if ctx.is_cancelled() {
            drop(f);
            workspace.cleanup_now();
            ctx.finish(Verdict::Cancelled {
                summary: "速度测试已取消。".to_string(),
            });
            return;
        }
        let t0 = Instant::now();
        if let Err(e) = f.write_all_aligned(&buf1m) {
            ctx.finish(Verdict::Failed {
                error: format!("顺序写入测试失败: {}", e),
            });
            return;
        }
        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (SEQ_BLOCK as f64 / (1024.0 * 1024.0)) / dt;
        min_w = min_w.min(mb_s);
        max_w = max_w.max(mb_s);
        written += SEQ_BLOCK as u64;

        let emit = last_sample.elapsed().as_millis() >= 80 || written >= target_bytes;
        if emit {
            last_sample = Instant::now();
        }
        let avg = (written as f64 / (1024.0 * 1024.0)) / t_start.elapsed().as_secs_f64().max(1e-6);
        ctx.update(|s| {
            s.bytes_written = written;
            s.current_speed_mb_s = mb_s;
            s.avg_write_mb_s = avg;
            s.progress = 0.30 * (written as f32 / target_bytes as f32);
            if emit {
                s.write_curve.push(SpeedSample {
                    x: written as f64 / (1024.0 * 1024.0 * 1024.0),
                    mb_per_sec: mb_s,
                });
            }
        });
    }
    let _ = f.flush_to_media();
    let seq_w_secs = t_start.elapsed().as_secs_f64().max(1e-6);
    metrics.seq_write_mb_s = (written as f64 / (1024.0 * 1024.0)) / seq_w_secs;
    metrics.seq_write_min_mb_s = if min_w < f64::MAX { min_w } else { 0.0 };
    metrics.seq_write_max_mb_s = max_w;
    drop(f);

    ctx.log(format!(
        "顺序 1MB 写入完成：平均 {:.2} MB/s（最低 {:.2} / 最高 {:.2} MB/s）",
        metrics.seq_write_mb_s, metrics.seq_write_min_mb_s, metrics.seq_write_max_mb_s
    ));
    ctx.update(|s| {
        s.benchmark = metrics.clone();
        s.phase_label = "阶段 2/6：顺序 1MB 无缓冲读取速度测试...".to_string();
    });

    // ---- 2. Sequential 1 MiB Read ----
    let mut rf = match DirectFile::open_read(&seq_path) {
        Ok(f) => f,
        Err(e) => {
            ctx.finish(Verdict::Failed {
                error: format!("打开测速文件读取失败: {}", e),
            });
            return;
        }
    };
    let mut read_bytes = 0u64;
    let mut min_r = f64::MAX;
    let mut max_r = 0.0f64;
    let t_start = Instant::now();
    let mut last_sample = Instant::now();

    while read_bytes < target_bytes {
        if ctx.is_cancelled() {
            drop(rf);
            workspace.cleanup_now();
            ctx.finish(Verdict::Cancelled {
                summary: "速度测试已取消。".to_string(),
            });
            return;
        }
        let t0 = Instant::now();
        if let Err(e) = rf.read_exact_aligned(&mut buf1m) {
            ctx.finish(Verdict::Failed {
                error: format!("顺序读取测试失败: {}", e),
            });
            return;
        }
        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (SEQ_BLOCK as f64 / (1024.0 * 1024.0)) / dt;
        min_r = min_r.min(mb_s);
        max_r = max_r.max(mb_s);
        read_bytes += SEQ_BLOCK as u64;

        let emit = last_sample.elapsed().as_millis() >= 80 || read_bytes >= target_bytes;
        if emit {
            last_sample = Instant::now();
        }
        let avg = (read_bytes as f64 / (1024.0 * 1024.0)) / t_start.elapsed().as_secs_f64().max(1e-6);
        ctx.update(|s| {
            s.bytes_verified = read_bytes;
            s.current_speed_mb_s = mb_s;
            s.avg_read_mb_s = avg;
            s.progress = 0.30 + 0.30 * (read_bytes as f32 / target_bytes as f32);
            if emit {
                s.read_curve.push(SpeedSample {
                    x: read_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    mb_per_sec: mb_s,
                });
            }
        });
    }
    let seq_r_secs = t_start.elapsed().as_secs_f64().max(1e-6);
    metrics.seq_read_mb_s = (read_bytes as f64 / (1024.0 * 1024.0)) / seq_r_secs;
    metrics.seq_read_min_mb_s = if min_r < f64::MAX { min_r } else { 0.0 };
    metrics.seq_read_max_mb_s = max_r;
    drop(rf);

    ctx.log(format!(
        "顺序 1MB 读取完成：平均 {:.2} MB/s（最低 {:.2} / 最高 {:.2} MB/s）",
        metrics.seq_read_mb_s, metrics.seq_read_min_mb_s, metrics.seq_read_max_mb_s
    ));

    // ---- 3 & 4. 64 KiB Block Write & Read (128 MiB cap or 2.5s cap) ----
    let med_total = target_bytes.min(128 * MIB);
    let mut buf64k = AlignedBuffer::new(MED_BLOCK);
    fill_buffer(&mut buf64k, run_id ^ 0x6464, 0);

    ctx.update(|s| {
        s.benchmark = metrics.clone();
        s.phase_label = "阶段 3/6：64KB 中小块写入测试...".to_string();
        s.progress = 0.62;
    });
    if let Ok(mut f64) = DirectFile::open_rw(&seq_path) {
        let mut done = 0u64;
        let t0 = Instant::now();
        while done < med_total && t0.elapsed().as_secs_f64() < 4.0 {
            if ctx.is_cancelled() {
                drop(f64);
                workspace.cleanup_now();
                ctx.finish(Verdict::Cancelled {
                    summary: "速度测试已取消。".to_string(),
                });
                return;
            }
            if f64.write_all_aligned(&buf64k).is_err() {
                break;
            }
            done += MED_BLOCK as u64;
        }
        let _ = f64.flush_to_media();
        metrics.block64k_write_mb_s =
            (done as f64 / (1024.0 * 1024.0)) / t0.elapsed().as_secs_f64().max(1e-6);
    }

    ctx.update(|s| {
        s.benchmark = metrics.clone();
        s.phase_label = "阶段 4/6：64KB 中小块读取测试...".to_string();
        s.progress = 0.72;
    });
    if let Ok(mut f64) = DirectFile::open_read(&seq_path) {
        let mut done = 0u64;
        let t0 = Instant::now();
        while done < med_total && t0.elapsed().as_secs_f64() < 4.0 {
            if ctx.is_cancelled() {
                drop(f64);
                workspace.cleanup_now();
                ctx.finish(Verdict::Cancelled {
                    summary: "速度测试已取消。".to_string(),
                });
                return;
            }
            if f64.read_exact_aligned(&mut buf64k).is_err() {
                break;
            }
            done += MED_BLOCK as u64;
        }
        metrics.block64k_read_mb_s =
            (done as f64 / (1024.0 * 1024.0)) / t0.elapsed().as_secs_f64().max(1e-6);
    }

    ctx.log(format!(
        "64KB 块读写完成：写入 {:.2} MB/s | 读取 {:.2} MB/s",
        metrics.block64k_write_mb_s, metrics.block64k_read_mb_s
    ));

    // ---- 5 & 6. 4 KiB Random Write & Read (QD1, ~4 seconds or 4096 ops max) ----
    let max_4k_slots = (target_bytes / RAND_BLOCK as u64).max(1);
    let mut buf4k = AlignedBuffer::new(RAND_BLOCK);
    fill_buffer(&mut buf4k, run_id ^ 0x4096, 0);
    let mut rng = run_id | 1;
    let mut next_offset = || -> u64 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % max_4k_slots) * (RAND_BLOCK as u64)
    };

    ctx.update(|s| {
        s.benchmark = metrics.clone();
        s.phase_label = "阶段 5/6：4K 随机写入测试 (QD1)...".to_string();
        s.progress = 0.80;
    });
    if let Ok(mut f4k) = DirectFile::open_rw(&seq_path) {
        let mut ops = 0u64;
        let t0 = Instant::now();
        while ops < 2048 && t0.elapsed().as_secs_f64() < 4.0 {
            if ctx.is_cancelled() {
                drop(f4k);
                workspace.cleanup_now();
                ctx.finish(Verdict::Cancelled {
                    summary: "速度测试已取消。".to_string(),
                });
                return;
            }
            let off = next_offset();
            if f4k.write_at(off, &buf4k).is_err() {
                break;
            }
            ops += 1;
        }
        let _ = f4k.flush_to_media();
        let secs = t0.elapsed().as_secs_f64().max(1e-6);
        metrics.rand4k_write_iops = ops as f64 / secs;
        metrics.rand4k_write_mb_s = (ops as f64 * 4.0 / 1024.0) / secs;
        metrics.rand4k_write_lat_ms = if ops > 0 { (secs * 1000.0) / ops as f64 } else { 0.0 };
    }

    ctx.update(|s| {
        s.benchmark = metrics.clone();
        s.phase_label = "阶段 6/6：4K 随机读取测试 (QD1)...".to_string();
        s.progress = 0.90;
    });
    if let Ok(mut f4k) = DirectFile::open_read(&seq_path) {
        let mut ops = 0u64;
        let t0 = Instant::now();
        while ops < 4096 && t0.elapsed().as_secs_f64() < 4.0 {
            if ctx.is_cancelled() {
                drop(f4k);
                workspace.cleanup_now();
                ctx.finish(Verdict::Cancelled {
                    summary: "速度测试已取消。".to_string(),
                });
                return;
            }
            let off = next_offset();
            if f4k.read_at(off, &mut buf4k).is_err() {
                break;
            }
            ops += 1;
        }
        let secs = t0.elapsed().as_secs_f64().max(1e-6);
        metrics.rand4k_read_iops = ops as f64 / secs;
        metrics.rand4k_read_mb_s = (ops as f64 * 4.0 / 1024.0) / secs;
        metrics.rand4k_read_lat_ms = if ops > 0 { (secs * 1000.0) / ops as f64 } else { 0.0 };
    }

    ctx.log(format!(
        "4K 随机读写完成：写入 {:.2} MB/s ({:.0} IOPS, {:.2} ms) | 读取 {:.2} MB/s ({:.0} IOPS, {:.2} ms)",
        metrics.rand4k_write_mb_s,
        metrics.rand4k_write_iops,
        metrics.rand4k_write_lat_ms,
        metrics.rand4k_read_mb_s,
        metrics.rand4k_read_iops,
        metrics.rand4k_read_lat_ms
    ));

    ctx.update(|s| {
        s.benchmark = metrics.clone();
        s.progress = 1.0;
        s.phase_label = "正在清理临时测速文件...".to_string();
    });
    workspace.cleanup_now();

    let summary = format!(
        "基准测试完成：顺序写 {:.1} MB/s | 顺序读 {:.1} MB/s | 4K 随机写 {:.2} MB/s ({:.0} IOPS) | 4K 随机读 {:.2} MB/s ({:.0} IOPS)",
        metrics.seq_write_mb_s,
        metrics.seq_read_mb_s,
        metrics.rand4k_write_mb_s,
        metrics.rand4k_write_iops,
        metrics.rand4k_read_mb_s,
        metrics.rand4k_read_iops
    );
    ctx.finish(Verdict::BenchmarkComplete { summary });
}
