pub mod aging;
pub mod benchmark;
pub mod error_scan;
pub mod full_verify;
pub mod pattern;
pub mod quick_expand;
pub mod raw_probe;
pub mod report;

use crate::disk::DiskInfo;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestMode {
    QuickExpand,
    FullVerify,
    ErrorScan,
    Benchmark,
    Aging,
}

impl TestMode {
    pub fn title_zh(&self) -> &'static str {
        match self {
            TestMode::QuickExpand => "快速扩容测试",
            TestMode::FullVerify => "全盘完整性校验 & 坏块扫描",
            TestMode::ErrorScan => "错误扫描 (HD Tune Pro 模式)",
            TestMode::Benchmark => "读写速度基准测试",
            TestMode::Aging => "老化 / 拷机测试",
        }
    }

    pub fn description_zh(&self) -> &'static str {
        match self {
            TestMode::QuickExpand => {
                "无需耗费数小时写满全盘，通过跨容量边界探针与二分定位快速识别扩容盘/假容量 U 盘并估算真实容量。"
            }
            TestMode::FullVerify => {
                "绕过系统缓存向目标空间写入带校验码的随机数据并逐扇区回读校验，精准排查扩容、坏块、掉速与数据损坏。"
            }
            TestMode::ErrorScan => {
                "复刻 HD Tune Pro 经典纯只读表面坏道扫描（零写入、绝对不破坏数据）：支持完整扫描与快速扫描、自定义起止 GB 区间、高密度坏道方格图与精确 LBA 坏道坐标定位。"
            }
            TestMode::Benchmark => {
                "测量顺序 1MB 读写、64KB 中小块读写与 4K 随机读写（IOPS / 延迟），并绘制实时速度波动曲线以识别掉速。"
            }
            TestMode::Aging => {
                "对存储设备执行多轮循环无缓冲读写校验，持续压测闪存颗粒、主控稳定性与发热掉速幅度。"
            }
        }
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self, TestMode::ErrorScan)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockState {
    Untested,
    Writing,
    Written,
    Good,
    Slow,
    Bad,
    Wrapped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockCell {
    pub index: usize,
    pub state: BlockState,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub write_mb_s: f32,
    pub read_mb_s: f32,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SpeedSample {
    /// Progress position in GiB (or elapsed seconds for aging/benchmark)
    pub x: f64,
    pub mb_per_sec: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BenchmarkMetrics {
    pub seq_write_mb_s: f64,
    pub seq_read_mb_s: f64,
    pub seq_write_min_mb_s: f64,
    pub seq_write_max_mb_s: f64,
    pub seq_read_min_mb_s: f64,
    pub seq_read_max_mb_s: f64,
    pub block64k_write_mb_s: f64,
    pub block64k_read_mb_s: f64,
    pub rand4k_write_mb_s: f64,
    pub rand4k_write_iops: f64,
    pub rand4k_write_lat_ms: f64,
    pub rand4k_read_mb_s: f64,
    pub rand4k_read_iops: f64,
    pub rand4k_read_lat_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgingCycleRecord {
    pub cycle: u32,
    pub write_mb_s: f64,
    pub read_mb_s: f64,
    pub errors: u64,
    pub duration_secs: f64,
}

/// Detailed record of a damaged or severely slow sector found during HD Tune Pro Error Scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BadSectorEntry {
    pub block_index: usize,
    pub offset_bytes: u64,
    pub lba_512: u64,
    pub speed_mb_s: f64,
    pub is_hard_error: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Verdict {
    Running,
    Healthy {
        verified_bytes: u64,
        summary: String,
    },
    FakeCapacity {
        reported_bytes: u64,
        estimated_real_bytes: u64,
        reason: String,
    },
    BadBlocks {
        verified_bytes: u64,
        bad_count: usize,
        slow_count: usize,
        summary: String,
    },
    BenchmarkComplete {
        summary: String,
    },
    AgingComplete {
        cycles_done: u32,
        total_errors: u64,
        throttle_pct: f64,
        summary: String,
    },
    Cancelled {
        summary: String,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    pub mode: TestMode,
    /// Target test size in MiB. `0` means auto-use all available free space (minus safety margin).
    pub target_size_mb: u64,
    /// Max single file size in MiB (default 1024 MiB = 1 GiB so FAT32 4GiB limit is never hit).
    pub chunk_size_mb: u64,
    /// Number of cells in the visual block grid map (e.g. 400 = 20x20).
    pub grid_blocks: usize,
    /// Speed threshold (MB/s) below which a verified block is marked "Slow / Warning".
    pub slow_threshold_mb_s: f64,
    /// Aging test cycle count (`0` = infinite until user stops).
    pub aging_cycles: u32,
    /// Automatically delete temporary test files when finished or cancelled.
    pub auto_cleanup: bool,
    /// Use elevated Raw LBA non-destructive probing in QuickExpand mode (5-15s for any capacity!).
    pub use_raw_lba_probe: bool,
    /// HD Tune Pro Error Scan: Quick Scan checkbox (samples each grid cell instead of reading every byte).
    pub error_scan_quick: bool,
    /// HD Tune Pro Error Scan: Start offset in GiB (0 = beginning of disk).
    pub error_scan_start_gb: f64,
    /// HD Tune Pro Error Scan: End offset in GiB (0 = full end of disk).
    pub error_scan_end_gb: f64,
    /// Reserve this many MiB free on the drive to keep the filesystem healthy.
    pub safety_margin_mb: u64,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            mode: TestMode::QuickExpand,
            target_size_mb: 0,
            chunk_size_mb: 1024,
            grid_blocks: 128,
            slow_threshold_mb_s: 5.0,
            aging_cycles: 5,
            auto_cleanup: true,
            use_raw_lba_probe: true,
            error_scan_quick: false,
            error_scan_start_gb: 0.0,
            error_scan_end_gb: 0.0,
            safety_margin_mb: 64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveTestSnapshot {
    pub disk: DiskInfo,
    pub config: TestConfig,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub running: bool,
    pub phase_label: String,
    pub progress: f32,
    pub elapsed_secs: f64,
    pub eta_secs: Option<f64>,
    pub bytes_written: u64,
    pub bytes_verified: u64,
    pub target_bytes: u64,
    pub current_speed_mb_s: f64,
    pub avg_write_mb_s: f64,
    pub avg_read_mb_s: f64,
    pub direct_io_active: bool,
    pub blocks: Vec<BlockCell>,
    pub write_curve: Vec<SpeedSample>,
    pub read_curve: Vec<SpeedSample>,
    pub benchmark: BenchmarkMetrics,
    pub aging_records: Vec<AgingCycleRecord>,
    pub bad_sectors: Vec<BadSectorEntry>,
    pub verdict: Verdict,
    pub logs: Vec<String>,
}

impl LiveTestSnapshot {
    pub fn new(disk: DiskInfo, config: TestConfig) -> Self {
        Self {
            disk,
            config,
            started_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            finished_at: None,
            running: true,
            phase_label: "正在初始化测试环境...".to_string(),
            progress: 0.0,
            elapsed_secs: 0.0,
            eta_secs: None,
            bytes_written: 0,
            bytes_verified: 0,
            target_bytes: 0,
            current_speed_mb_s: 0.0,
            avg_write_mb_s: 0.0,
            avg_read_mb_s: 0.0,
            direct_io_active: false,
            blocks: Vec::new(),
            write_curve: Vec::new(),
            read_curve: Vec::new(),
            benchmark: BenchmarkMetrics::default(),
            aging_records: Vec::new(),
            bad_sectors: Vec::new(),
            verdict: Verdict::Running,
            logs: Vec::new(),
        }
    }

    pub fn push_log(&mut self, msg: impl AsRef<str>) {
        let ts = Local::now().format("%H:%M:%S");
        self.logs.push(format!("[{}] {}", ts, msg.as_ref()));
        if self.logs.len() > 2000 {
            self.logs.drain(0..200);
        }
    }

    pub fn count_blocks(&self) -> (usize, usize, usize, usize) {
        let mut good = 0;
        let mut slow = 0;
        let mut bad = 0;
        let mut wrapped = 0;
        for b in &self.blocks {
            match b.state {
                BlockState::Good => good += 1,
                BlockState::Slow => slow += 1,
                BlockState::Bad => bad += 1,
                BlockState::Wrapped => wrapped += 1,
                _ => {}
            }
        }
        (good, slow, bad, wrapped)
    }
}

pub struct EngineContext {
    pub state: Arc<Mutex<LiveTestSnapshot>>,
    pub cancel: Arc<AtomicBool>,
    pub start_instant: Instant,
    pub repaint_cb: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl EngineContext {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn update<F: FnOnce(&mut LiveTestSnapshot)>(&self, f: F) {
        if let Ok(mut guard) = self.state.lock() {
            guard.elapsed_secs = self.start_instant.elapsed().as_secs_f64();
            f(&mut guard);
        }
        if let Some(ref cb) = self.repaint_cb {
            cb();
        }
    }

    pub fn log(&self, msg: impl AsRef<str>) {
        self.update(|s| s.push_log(msg));
    }

    pub fn finish(&self, verdict: Verdict) {
        self.update(|s| {
            s.running = false;
            s.finished_at = Some(Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
            s.eta_secs = None;
            s.current_speed_mb_s = 0.0;
            s.verdict = verdict;
        });
    }
}

/// RAII guard that ensures our temporary test folder is deleted when a test ends or is cancelled.
pub struct TempWorkspace {
    pub dir: PathBuf,
    auto_cleanup: bool,
}

impl TempWorkspace {
    pub fn create(mount_point: &Path, run_id: u64, auto_cleanup: bool) -> std::io::Result<Self> {
        let dir = mount_point.join(format!(".mydisktest_run_{:08x}", run_id & 0xFFFF_FFFF));
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, auto_cleanup })
    }

    pub fn chunk_path(&self, index: usize) -> PathBuf {
        self.dir.join(format!("mdt_{:04}.bin", index))
    }

    pub fn cleanup_now(&self) {
        if self.auto_cleanup && self.dir.exists() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        self.cleanup_now();
    }
}

/// Computes the safe usable test target bytes (aligned to 1 MiB) based on drive free space & config.
pub fn compute_target_bytes(mount_point: &Path, config: &TestConfig, fallback_mb: u64) -> Result<u64, String> {
    let (_, free_bytes) = crate::disk::direct_io::get_volume_space(mount_point)
        .map_err(|e| format!("无法获取目标磁盘可用空间: {}", e))?;

    let margin = config.safety_margin_mb * 1024 * 1024;
    if free_bytes <= margin + 16 * 1024 * 1024 {
        return Err(format!(
            "磁盘可用空间不足（当前可用 {}），请清理空间后再测试。",
            crate::disk::format_bytes(free_bytes)
        ));
    }
    let usable = free_bytes - margin;
    let desired = if config.target_size_mb == 0 {
        if fallback_mb > 0 {
            usable.min(fallback_mb * 1024 * 1024)
        } else {
            usable
        }
    } else {
        usable.min(config.target_size_mb * 1024 * 1024)
    };

    // Align down to 1 MiB
    let aligned = (desired / (1024 * 1024)) * (1024 * 1024);
    if aligned < 16 * 1024 * 1024 {
        return Err("有效测试容量小于 16 MiB，无法开始测试。".to_string());
    }
    Ok(aligned)
}

/// Spawns a background worker thread executing the chosen MyDiskTest mode.
pub fn spawn_test(
    disk: DiskInfo,
    config: TestConfig,
    repaint_cb: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (Arc<Mutex<LiveTestSnapshot>>, Arc<AtomicBool>) {
    let snapshot = LiveTestSnapshot::new(disk.clone(), config.clone());
    let state = Arc::new(Mutex::new(snapshot));
    let cancel = Arc::new(AtomicBool::new(false));

    let ctx = EngineContext {
        state: Arc::clone(&state),
        cancel: Arc::clone(&cancel),
        start_instant: Instant::now(),
        repaint_cb,
    };

    std::thread::spawn(move || {
        match config.mode {
            TestMode::QuickExpand => quick_expand::run(&ctx, &disk, &config),
            TestMode::FullVerify => full_verify::run(&ctx, &disk, &config),
            TestMode::ErrorScan => error_scan::run(&ctx, &disk, &config),
            TestMode::Benchmark => benchmark::run(&ctx, &disk, &config),
            TestMode::Aging => aging::run(&ctx, &disk, &config),
        }
    });

    (state, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::disk_info_for_custom_path;

    fn run_mode_sync(mode: TestMode, size_mb: u64, aging_cycles: u32) -> LiveTestSnapshot {
        let dir = std::env::temp_dir().join(format!(
            "mydisktest_e2e_{:?}_{:x}",
            mode,
            pattern::generate_run_id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let disk = disk_info_for_custom_path(dir.clone());
        let config = TestConfig {
            mode,
            target_size_mb: size_mb,
            chunk_size_mb: 64,
            grid_blocks: 16,
            slow_threshold_mb_s: 0.1,
            aging_cycles,
            auto_cleanup: true,
            use_raw_lba_probe: false,
            error_scan_quick: true,
            error_scan_start_gb: 0.0,
            error_scan_end_gb: 0.0,
            safety_margin_mb: 16,
        };
        let (state, _cancel) = spawn_test(disk, config, None);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let snap = state.lock().unwrap().clone();
            if !snap.running {
                // Verify auto-cleanup left no leftover temp folders inside dir
                let remaining: Vec<_> = std::fs::read_dir(&dir)
                    .unwrap()
                    .filter_map(|e| e.ok())
                    .collect();
                assert!(
                    remaining.is_empty(),
                    "Expected temp workspace to be cleaned up, found: {:?}",
                    remaining
                );
                let _ = std::fs::remove_dir_all(&dir);
                return snap;
            }
        }
    }

    #[test]
    fn e2e_quick_expand_and_report() {
        let snap = run_mode_sync(TestMode::QuickExpand, 16, 1);
        assert!(
            matches!(snap.verdict, Verdict::Healthy { .. }),
            "Unexpected verdict: {:?}",
            snap.verdict
        );
        let md = report::format_markdown_report(&snap);
        assert!(md.contains("MyDiskTest 磁盘检测报告"));
        let json = report::format_json_report(&snap).expect("json report");
        assert!(json.contains("QuickExpand"));
    }

    #[test]
    fn e2e_full_verify() {
        let snap = run_mode_sync(TestMode::FullVerify, 16, 1);
        assert!(
            matches!(snap.verdict, Verdict::Healthy { .. }),
            "Unexpected verdict: {:?}",
            snap.verdict
        );
        assert!(!snap.blocks.is_empty());
        assert_eq!(snap.bytes_written, 16 * 1024 * 1024);
        assert_eq!(snap.bytes_verified, 16 * 1024 * 1024);
    }

    #[test]
    fn e2e_benchmark() {
        let snap = run_mode_sync(TestMode::Benchmark, 16, 1);
        assert!(
            matches!(snap.verdict, Verdict::BenchmarkComplete { .. }),
            "Unexpected verdict: {:?}",
            snap.verdict
        );
        assert!(snap.benchmark.seq_write_mb_s > 0.0);
        assert!(snap.benchmark.seq_read_mb_s > 0.0);
        assert!(snap.benchmark.rand4k_read_iops > 0.0);
    }

    #[test]
    fn e2e_aging_cycles() {
        let snap = run_mode_sync(TestMode::Aging, 16, 2);
        assert!(
            matches!(
                snap.verdict,
                Verdict::AgingComplete {
                    cycles_done: 2,
                    total_errors: 0,
                    ..
                }
            ),
            "Unexpected verdict: {:?}",
            snap.verdict
        );
        assert_eq!(snap.aging_records.len(), 2);
    }
}
