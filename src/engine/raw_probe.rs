use crate::disk::direct_io::{AlignedBuffer, DirectFile};
use crate::disk::{format_bytes, format_bytes_decimal, DiskInfo};
use crate::engine::pattern::{fill_buffer, generate_run_id, verify_buffer, VerifyStatus};
use crate::engine::{
    BlockCell, BlockState, EngineContext, SpeedSample, TestConfig, Verdict,
};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const PROBE_BLOCK_SIZE: usize = 64 * 1024; // 64 KiB (16 x 4KB aligned sectors)
const SAFE_START_OFFSET: u64 = 32 * 1024 * 1024; // Skip first 32 MiB (MBR/GPT/EFI/FAT/exFAT headers)
const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ProbeEvent {
    Init {
        disk_bytes: u64,
        offsets: Vec<u64>,
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
        write_mb_s: f32,
        read_mb_s: f32,
        progress: f32,
        bytes_written: u64,
        bytes_verified: u64,
        current_mb_s: f64,
        avg_write_mb_s: f64,
        avg_read_mb_s: f64,
        curve_write: Option<SpeedSample>,
        curve_read: Option<SpeedSample>,
        note: Option<String>,
    },
    Done {
        verdict: Verdict,
    },
}

/// Launches the elevated `--raw-probe-worker` subprocess and streams live JSONL updates into `ctx`.
pub fn run_raw_lba_probe(ctx: &EngineContext, disk: &DiskInfo, config: &TestConfig) {
    let Some(ref raw_path) = disk.raw_disk_path else {
        ctx.finish(Verdict::Failed {
            error: "该设备不支持底层 LBA 探针，请取消勾选「极速底层 LBA 探针」改用安全文件模式。".to_string(),
        });
        return;
    };

    if disk.is_system {
        ctx.finish(Verdict::Failed {
            error: "安全保护：禁止对系统启动盘执行底层 LBA 探针！".to_string(),
        });
        return;
    }

    let disk_bytes = disk.physical_bytes.max(disk.total_bytes);
    if disk_bytes < 128 * MIB {
        ctx.finish(Verdict::Failed {
            error: "设备容量小于 128 MiB，请改用安全文件模式测试。".to_string(),
        });
        return;
    }

    let run_id = generate_run_id();
    let status_file = std::env::temp_dir().join(format!("mdt_raw_{:08x}.jsonl", run_id & 0xFFFF_FFFF));
    let cancel_file = std::env::temp_dir().join(format!("mdt_raw_{:08x}.cancel", run_id & 0xFFFF_FFFF));
    let _ = std::fs::remove_file(&status_file);
    let _ = std::fs::remove_file(&cancel_file);
    // Create empty status file readable by current user before spawning elevated worker
    if let Err(e) = File::create(&status_file) {
        ctx.finish(Verdict::Failed {
            error: format!("无法创建进度通信文件: {}", e),
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

    let num_probes = config.grid_blocks.clamp(64, 256);
    ctx.update(|s| {
        s.direct_io_active = true;
        s.target_bytes = disk_bytes;
        s.phase_label = "正在请求系统管理员授权以进行无损底层 LBA 极速探测...".to_string();
    });
    ctx.log(format!(
        "启动极速底层 LBA 探针模式：设备 {}（标称 {}），计划采样 {} 个跨区探针（总 I/O 仅约 {}）",
        raw_path,
        format_bytes_decimal(disk_bytes),
        num_probes,
        format_bytes((num_probes as u64) * (PROBE_BLOCK_SIZE as u64) * 4)
    ));
    ctx.log("安全机制：跳过前 32 MiB 分区表与文件系统元数据区，写前完整备份并在测后逆序无损还原，测试期间磁盘会短暂卸载并在完成后自动重新挂载。");

    let mut child = match spawn_elevated_worker(
        &exe_path,
        raw_path,
        disk_bytes,
        num_probes,
        config.slow_threshold_mb_s,
        &status_file,
        &cancel_file,
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&status_file);
            ctx.log(format!("无法启动授权探针进程: {}", e));
            ctx.finish(Verdict::Failed { error: e });
            return;
        }
    };

    // Tail the JSONL status file until worker exits
    let mut lines_read = 0usize;
    let mut got_done = false;
    let mut sent_cancel = false;

    loop {
        if ctx.is_cancelled() && !sent_cancel {
            sent_cancel = true;
            let _ = File::create(&cancel_file);
            ctx.log("已发送取消信号，等待底层探针完成无损数据还原与重新挂载...");
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
                    if let Ok(ev) = serde_json::from_str::<ProbeEvent>(trimmed) {
                        if apply_probe_event(ctx, ev) {
                            got_done = true;
                        }
                    }
                }
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                // One final drain of status file
                if let Ok(file) = File::open(&status_file) {
                    let reader = BufReader::new(file);
                    for (idx, line_res) in reader.lines().enumerate() {
                        if idx < lines_read {
                            continue;
                        }
                        if let Ok(line) = line_res {
                            if let Ok(ev) = serde_json::from_str::<ProbeEvent>(line.trim()) {
                                if apply_probe_event(ctx, ev) {
                                    got_done = true;
                                }
                            }
                        }
                    }
                }

                if !got_done {
                    if ctx.is_cancelled() {
                        ctx.finish(Verdict::Cancelled {
                            summary: "底层 LBA 探针已取消并还原数据。".to_string(),
                        });
                    } else if !status.success() {
                        let msg = "管理员授权已取消或底层探针未能启动。您也可取消勾选「极速底层 LBA 探针」改用免授权文件模式。".to_string();
                        ctx.log(&msg);
                        ctx.finish(Verdict::Failed { error: msg });
                    } else {
                        ctx.finish(Verdict::Failed {
                            error: "底层探针进程提前退出，未返回检测结论。".to_string(),
                        });
                    }
                }
                break;
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(e) => {
                ctx.finish(Verdict::Failed {
                    error: format!("等待探针进程失败: {}", e),
                });
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&status_file);
    let _ = std::fs::remove_file(&cancel_file);
}

fn apply_probe_event(ctx: &EngineContext, ev: ProbeEvent) -> bool {
    match ev {
        ProbeEvent::Init { disk_bytes, offsets } => {
            let count = offsets.len().max(1);
            let span = disk_bytes / count as u64;
            let cells: Vec<BlockCell> = offsets
                .into_iter()
                .enumerate()
                .map(|(i, off)| BlockCell {
                    index: i,
                    state: BlockState::Untested,
                    offset_bytes: off,
                    size_bytes: span.max(PROBE_BLOCK_SIZE as u64),
                    write_mb_s: 0.0,
                    read_mb_s: 0.0,
                    note: None,
                })
                .collect();
            ctx.update(|s| {
                s.target_bytes = disk_bytes;
                s.blocks = cells;
            });
            false
        }
        ProbeEvent::Phase { label } => {
            ctx.update(|s| s.phase_label = label);
            false
        }
        ProbeEvent::Log { msg } => {
            ctx.log(msg);
            false
        }
        ProbeEvent::CellUpdate {
            index,
            state,
            write_mb_s,
            read_mb_s,
            progress,
            bytes_written,
            bytes_verified,
            current_mb_s,
            avg_write_mb_s,
            avg_read_mb_s,
            curve_write,
            curve_read,
            note,
        } => {
            ctx.update(|s| {
                s.progress = progress;
                s.bytes_written = bytes_written;
                s.bytes_verified = bytes_verified;
                s.current_speed_mb_s = current_mb_s;
                if avg_write_mb_s > 0.0 {
                    s.avg_write_mb_s = avg_write_mb_s;
                }
                if avg_read_mb_s > 0.0 {
                    s.avg_read_mb_s = avg_read_mb_s;
                }
                if let Some(cell) = s.blocks.get_mut(index) {
                    cell.state = state;
                    if write_mb_s > 0.0 {
                        cell.write_mb_s = write_mb_s;
                    }
                    if read_mb_s > 0.0 {
                        cell.read_mb_s = read_mb_s;
                    }
                    if note.is_some() {
                        cell.note = note;
                    }
                }
                if let Some(pt) = curve_write {
                    s.write_curve.push(pt);
                }
                if let Some(pt) = curve_read {
                    s.read_curve.push(pt);
                }
            });
            false
        }
        ProbeEvent::Done { verdict } => {
            ctx.finish(verdict);
            true
        }
    }
}

#[cfg(target_os = "macos")]
fn spawn_elevated_worker(
    exe_path: &Path,
    raw_path: &str,
    disk_bytes: u64,
    num_probes: usize,
    slow_threshold: f64,
    status_file: &Path,
    cancel_file: &Path,
) -> Result<std::process::Child, String> {
    let sh_quote = |s: &str| -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    };
    let inner_cmd = format!(
        "{} --raw-probe-worker {} {} {} {:.2} {} {}",
        sh_quote(&exe_path.to_string_lossy()),
        sh_quote(raw_path),
        disk_bytes,
        num_probes,
        slow_threshold,
        sh_quote(&status_file.to_string_lossy()),
        sh_quote(&cancel_file.to_string_lossy()),
    );
    let escaped_for_as = inner_cmd.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"MyDiskTest 需要管理员权限以对外置存储执行 5~15 秒无损底层扩容探测：\"",
        escaped_for_as
    );

    Command::new("osascript")
        .args(["-e", &script])
        .spawn()
        .map_err(|e| format!("无法调用 macOS 授权弹窗 (osascript): {}", e))
}

#[cfg(target_os = "windows")]
fn spawn_elevated_worker(
    exe_path: &Path,
    raw_path: &str,
    disk_bytes: u64,
    num_probes: usize,
    slow_threshold: f64,
    status_file: &Path,
    cancel_file: &Path,
) -> Result<std::process::Child, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let args_str = format!(
        "--raw-probe-worker \"{}\" {} {} {:.2} \"{}\" \"{}\"",
        raw_path,
        disk_bytes,
        num_probes,
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
fn spawn_elevated_worker(
    exe_path: &Path,
    raw_path: &str,
    disk_bytes: u64,
    num_probes: usize,
    slow_threshold: f64,
    status_file: &Path,
    cancel_file: &Path,
) -> Result<std::process::Child, String> {
    Command::new(exe_path)
        .args([
            "--raw-probe-worker",
            raw_path,
            &disk_bytes.to_string(),
            &num_probes.to_string(),
            &format!("{:.2}", slow_threshold),
            &status_file.to_string_lossy(),
            &cancel_file.to_string_lossy(),
        ])
        .spawn()
        .map_err(|e| format!("启动底层探针进程失败: {}", e))
}

/// Entrypoint when invoked as `--raw-probe-worker <raw_path> <disk_bytes> <num_probes> <slow_mb_s> <status_file> <cancel_file>`.
pub fn run_cli_worker(args: &[String]) -> i32 {
    if args.len() < 6 {
        eprintln!("Invalid --raw-probe-worker arguments");
        return 2;
    }
    let raw_path = &args[0];
    let disk_bytes: u64 = args[1].parse().unwrap_or(0);
    let num_probes: usize = args[2].parse().unwrap_or(128).clamp(32, 256);
    let slow_threshold: f64 = args[3].parse().unwrap_or(5.0);
    let status_path = PathBuf::from(&args[4]);
    let cancel_path = PathBuf::from(&args[5]);

    let emit = |ev: &ProbeEvent| {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&status_path) {
            if let Ok(line) = serde_json::to_string(ev) {
                let _ = writeln!(f, "{}", line);
                let _ = f.flush();
            }
        }
    };

    // Hard safety guard: never allow probing disk0 / PhysicalDrive0!
    if raw_path == "/dev/rdisk0"
        || raw_path == "/dev/disk0"
        || raw_path.eq_ignore_ascii_case(r"\\.\PhysicalDrive0")
        || disk_bytes < 128 * MIB
    {
        emit(&ProbeEvent::Done {
            verdict: Verdict::Failed {
                error: format!("安全拦截：拒绝底层探测系统盘或无效设备 {}", raw_path),
            },
        });
        return 1;
    }

    // macOS: unmount all volumes on /dev/diskN so /dev/rdiskN can be opened for raw write,
    // and guarantee remount on drop!
    #[cfg(target_os = "macos")]
    let _remount_guard = {
        let bsd_whole = raw_path.replace("/dev/rdisk", "/dev/disk");
        emit(&ProbeEvent::Phase {
            label: format!("阶段 1/5：临时卸载 {} 卷以获取独占底层读写权限...", bsd_whole),
        });
        let out = Command::new("diskutil")
            .args(["unmountDisk", &bsd_whole])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                emit(&ProbeEvent::Log {
                    msg: format!("已临时卸载 {}（测试完成后将自动重新挂载）", bsd_whole),
                });
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                emit(&ProbeEvent::Done {
                    verdict: Verdict::Failed {
                        error: format!("卸载磁盘失败（可能有文件正在占用该 U 盘）: {}", err),
                    },
                });
                return 1;
            }
            Err(e) => {
                emit(&ProbeEvent::Done {
                    verdict: Verdict::Failed {
                        error: format!("调用 diskutil 失败: {}", e),
                    },
                });
                return 1;
            }
        }
        RemountGuard { bsd_whole }
    };

    let offsets = build_probe_offsets(disk_bytes, num_probes);
    emit(&ProbeEvent::Init {
        disk_bytes,
        offsets: offsets.clone(),
    });

    let mut dev = match DirectFile::open_rw(Path::new(raw_path)) {
        Ok(f) => f,
        Err(e) => {
            #[cfg(target_os = "macos")]
            let msg = if e.raw_os_error() == Some(1) {
                format!(
                    "macOS 隐私保护拦截了底层块设备 {} 访问（Operation not permitted）。\n\
                    请点击下方「打开完全磁盘访问权限设置」按钮，在「系统设置 → 隐私与安全性 → 完全磁盘访问权限」中点击「+」添加并开启 MyDiskTest.app，重启本软件后即可实现 5~15 秒极速探测！",
                    raw_path
                )
            } else {
                format!("无法打开原始块设备 {} 进行读写: {}", raw_path, e)
            };
            #[cfg(not(target_os = "macos"))]
            let msg = format!("无法打开原始块设备 {} 进行读写: {}", raw_path, e);

            emit(&ProbeEvent::Done {
                verdict: Verdict::Failed { error: msg },
            });
            return 1;
        }
    };

    let run_id = generate_run_id();
    let count = offsets.len();

    // ---- Phase 1: Backup original 64 KiB sectors at every probe offset ----
    emit(&ProbeEvent::Phase {
        label: format!("阶段 2/5：无损备份全盘 {} 个探测点原始扇区...", count),
    });
    let mut backups: Vec<Option<Vec<u8>>> = vec![None; count];
    let mut rbuf = AlignedBuffer::new(PROBE_BLOCK_SIZE);
    for (i, &off) in offsets.iter().enumerate() {
        if cancel_path.exists() {
            emit(&ProbeEvent::Done {
                verdict: Verdict::Cancelled {
                    summary: "已在备份阶段取消，未写入任何数据，磁盘已重新挂载。".to_string(),
                },
            });
            return 0;
        }
        if dev.read_at(off, &mut rbuf).is_ok() {
            backups[i] = Some(rbuf.as_slice().to_vec());
        }
    }
    emit(&ProbeEvent::Log {
        msg: format!(
            "已完成 {} 个探测点原始扇区内存备份（共 {}）",
            count,
            format_bytes((count * PROBE_BLOCK_SIZE) as u64)
        ),
    });

    // ---- Phase 2: Write unique run_id + LBA_offset fingerprint probes ----
    emit(&ProbeEvent::Phase {
        label: format!("阶段 3/5：跨容量边界写入 {} 个唯一指纹探针...", count),
    });
    let mut wbuf = AlignedBuffer::new(PROBE_BLOCK_SIZE);
    let mut total_written = 0u64;
    let write_start = Instant::now();
    let mut wrote_indices = Vec::with_capacity(count);

    for (i, &off) in offsets.iter().enumerate() {
        if cancel_path.exists() {
            restore_backups(&mut dev, &offsets, &backups, &wrote_indices, &emit);
            emit(&ProbeEvent::Done {
                verdict: Verdict::Cancelled {
                    summary: "已取消探测，所有已触碰扇区均已逆序无损还原并重新挂载。".to_string(),
                },
            });
            return 0;
        }
        fill_buffer(&mut wbuf, run_id, off);
        let t0 = Instant::now();
        let res = dev.write_at(off, &wbuf);
        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (PROBE_BLOCK_SIZE as f64 / (1024.0 * 1024.0)) / dt;

        match res {
            Ok(()) => {
                wrote_indices.push(i);
                total_written += PROBE_BLOCK_SIZE as u64;
                let avg_w = (total_written as f64 / (1024.0 * 1024.0))
                    / write_start.elapsed().as_secs_f64().max(1e-6);
                emit(&ProbeEvent::CellUpdate {
                    index: i,
                    state: BlockState::Written,
                    write_mb_s: mb_s as f32,
                    read_mb_s: 0.0,
                    progress: 0.15 + 0.35 * ((i + 1) as f32 / count as f32),
                    bytes_written: total_written,
                    bytes_verified: 0,
                    current_mb_s: mb_s,
                    avg_write_mb_s: avg_w,
                    avg_read_mb_s: 0.0,
                    curve_write: Some(SpeedSample {
                        x: off as f64 / (1024.0 * 1024.0 * 1024.0),
                        mb_per_sec: mb_s,
                    }),
                    curve_read: None,
                    note: None,
                });
            }
            Err(e) => {
                emit(&ProbeEvent::Log {
                    msg: format!("偏移 {} 写入失败: {}", format_bytes(off), e),
                });
                emit(&ProbeEvent::CellUpdate {
                    index: i,
                    state: BlockState::Bad,
                    write_mb_s: 0.0,
                    read_mb_s: 0.0,
                    progress: 0.15 + 0.35 * ((i + 1) as f32 / count as f32),
                    bytes_written: total_written,
                    bytes_verified: 0,
                    current_mb_s: 0.0,
                    avg_write_mb_s: 0.0,
                    avg_read_mb_s: 0.0,
                    curve_write: None,
                    curve_read: None,
                    note: Some(format!("写入失败: {}", e)),
                });
            }
        }
    }
    let _ = dev.flush_to_media();

    // ---- Phase 3: Read back every probe in order to catch wrap-around & black-hole ----
    emit(&ProbeEvent::Phase {
        label: format!("阶段 4/5：直读回校全部 {} 个探针并定位真实容量...", count),
    });
    let mut total_verified = 0u64;
    let read_start = Instant::now();
    let mut good_count = 0usize;
    let mut slow_count = 0usize;
    let mut bad_count = 0usize;
    let mut wrapped_count = 0usize;
    let mut zero_or_ff_count = 0usize;
    let mut first_bad_offset: Option<u64> = None;
    let mut first_wrap_diff: Option<u64> = None;
    let mut last_good_offset: u64 = 0;
    let mut wrap_reason = String::new();

    for (i, &off) in offsets.iter().enumerate() {
        if cancel_path.exists() {
            restore_backups(&mut dev, &offsets, &backups, &wrote_indices, &emit);
            emit(&ProbeEvent::Done {
                verdict: Verdict::Cancelled {
                    summary: "已取消探测，所有扇区均已逆序无损还原并重新挂载。".to_string(),
                },
            });
            return 0;
        }
        let t0 = Instant::now();
        let res = dev.read_at(off, &mut rbuf);
        let dt = t0.elapsed().as_secs_f64().max(1e-6);
        let mb_s = (PROBE_BLOCK_SIZE as f64 / (1024.0 * 1024.0)) / dt;

        let (state, note) = match res {
            Ok(()) => {
                total_verified += PROBE_BLOCK_SIZE as u64;
                match verify_buffer(&rbuf, run_id, off) {
                    VerifyStatus::Ok => {
                        if first_bad_offset.is_none() && first_wrap_diff.is_none() {
                            last_good_offset = off + PROBE_BLOCK_SIZE as u64;
                        }
                        if mb_s < slow_threshold {
                            slow_count += 1;
                            (BlockState::Slow, Some(format!("校验通过，读取偏慢 ({:.1} MB/s)", mb_s)))
                        } else {
                            good_count += 1;
                            (BlockState::Good, None)
                        }
                    }
                    VerifyStatus::Wrapped {
                        expected_offset,
                        found_offset,
                    } => {
                        wrapped_count += 1;
                        if first_wrap_diff.is_none() {
                            let diff = found_offset.abs_diff(expected_offset);
                            first_wrap_diff = Some(if diff > 0 { diff } else { expected_offset });
                            wrap_reason = format!(
                                "在 LBA 偏移 {} 读到了偏移 {} 的探针数据（典型循环覆盖扩容盘！）",
                                format_bytes(expected_offset),
                                format_bytes(found_offset)
                            );
                            emit(&ProbeEvent::Log {
                                msg: format!("🚨 检出扩容回绕：{}", wrap_reason),
                            });
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
                        bad_count += 1;
                        zero_or_ff_count += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(off);
                            emit(&ProbeEvent::Log {
                                msg: format!("🚨 偏移 {} 起读回全 0x00（黑洞/丢弃型扩容盘）", format_bytes(off)),
                            });
                        }
                        (BlockState::Bad, Some("读回全 0x00（假容量黑洞区）".to_string()))
                    }
                    VerifyStatus::FFFilled => {
                        bad_count += 1;
                        zero_or_ff_count += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(off);
                            emit(&ProbeEvent::Log {
                                msg: format!("🚨 偏移 {} 起读回全 0xFF（假容量未映射区）", format_bytes(off)),
                            });
                        }
                        (BlockState::Bad, Some("读回全 0xFF（假容量未映射区）".to_string()))
                    }
                    VerifyStatus::Corrupted { reason } => {
                        bad_count += 1;
                        if first_bad_offset.is_none() {
                            first_bad_offset = Some(off);
                        }
                        (BlockState::Bad, Some(reason))
                    }
                }
            }
            Err(e) => {
                bad_count += 1;
                if first_bad_offset.is_none() {
                    first_bad_offset = Some(off);
                }
                (BlockState::Bad, Some(format!("读取失败: {}", e)))
            }
        };

        let avg_r = (total_verified as f64 / (1024.0 * 1024.0))
            / read_start.elapsed().as_secs_f64().max(1e-6);
        emit(&ProbeEvent::CellUpdate {
            index: i,
            state,
            write_mb_s: 0.0,
            read_mb_s: mb_s as f32,
            progress: 0.50 + 0.40 * ((i + 1) as f32 / count as f32),
            bytes_written: total_written,
            bytes_verified: total_verified,
            current_mb_s: mb_s,
            avg_write_mb_s: 0.0,
            avg_read_mb_s: avg_r,
            curve_write: None,
            curve_read: Some(SpeedSample {
                x: off as f64 / (1024.0 * 1024.0 * 1024.0),
                mb_per_sec: mb_s,
            }),
            note,
        });
    }

    // ---- Phase 3.5: If fake capacity detected, run quick binary search (<1s) to pinpoint exact real capacity! ----
    let is_fake = wrapped_count > 0 || (zero_or_ff_count >= 2 && bad_count >= 2);
    let mut precise_real_bytes = first_bad_offset
        .or(first_wrap_diff)
        .unwrap_or(last_good_offset);

    if is_fake && !cancel_path.exists() {
        emit(&ProbeEvent::Phase {
            label: "发现扩容异常，正在执行二分搜索精确定位真实闪存容量...".to_string(),
        });
        if let Some(pinpointed) = binary_search_real_capacity(&mut dev, disk_bytes, offsets[0]) {
            precise_real_bytes = pinpointed;
            emit(&ProbeEvent::Log {
                msg: format!(
                    "🎯 二分精确定位完成：该设备真实有效物理容量约 {}（标称 {}）",
                    format_bytes(pinpointed),
                    format_bytes_decimal(disk_bytes)
                ),
            });
        }
    }

    // ---- Phase 4: Reverse-Order Lossless Restore (ALWAYS!) ----
    restore_backups(&mut dev, &offsets, &backups, &wrote_indices, &emit);
    drop(dev);

    // Explicitly remount before reporting Done so user sees disk mounted immediately!
    #[cfg(target_os = "macos")]
    drop(_remount_guard);

    let verdict = if is_fake {
        let reason = if !wrap_reason.is_empty() {
            wrap_reason
        } else {
            format!(
                "从约 {} 起连续读回全 0x00 / 0xFF，共 {} 个异常区，判定为扩容盘/假容量设备！",
                format_bytes(precise_real_bytes),
                zero_or_ff_count
            )
        };
        Verdict::FakeCapacity {
            reported_bytes: disk_bytes,
            estimated_real_bytes: precise_real_bytes,
            reason,
        }
    } else if bad_count > 0 {
        Verdict::BadBlocks {
            verified_bytes: disk_bytes,
            bad_count,
            slow_count,
            summary: format!(
                "底层快速探测完成（覆盖全盘 {}）：正常 {} 块，慢速 {} 块，损坏 {} 块。建议运行「全盘完整性校验」进一步排查。",
                format_bytes_decimal(disk_bytes),
                good_count,
                slow_count,
                bad_count
            ),
        }
    } else {
        Verdict::Healthy {
            verified_bytes: disk_bytes,
            summary: format!(
                "极速底层 LBA 扩容检测通过！全盘 {} 共 {} 个跨区探针校验 100% 一致，未发现扩容回绕或黑洞区（慢速区 {} 个），原始扇区已无损还原。",
                format_bytes_decimal(disk_bytes),
                good_count + slow_count,
                slow_count
            ),
        }
    };

    emit(&ProbeEvent::Done { verdict });
    0
}

fn restore_backups<F: Fn(&ProbeEvent)>(
    dev: &mut DirectFile,
    offsets: &[u64],
    backups: &[Option<Vec<u8>>],
    wrote_indices: &[usize],
    emit: &F,
) {
    emit(&ProbeEvent::Phase {
        label: "阶段 5/5：逆序无损还原原始扇区数据并重新挂载磁盘...".to_string(),
    });
    let mut aligned = AlignedBuffer::new(PROBE_BLOCK_SIZE);
    // Reverse order: high LBAs first, low LBAs LAST so real data on genuine flash is never clobbered!
    for &idx in wrote_indices.iter().rev() {
        if let Some(ref orig) = backups[idx] {
            aligned.as_mut_slice().copy_from_slice(orig);
            let _ = dev.write_at(offsets[idx], &aligned);
        }
    }
    let _ = dev.flush_to_media();
    emit(&ProbeEvent::Log {
        msg: format!("已完成 {} 个探测点原始数据逆序无损还原。", wrote_indices.len()),
    });
}

/// Fast ~10-step lossless binary search to pinpoint true real capacity down to ~16 MiB.
fn binary_search_real_capacity(dev: &mut DirectFile, disk_bytes: u64, anchor: u64) -> Option<u64> {
    let mut lo = anchor;
    let mut hi = disk_bytes.saturating_sub(2 * MIB);
    let bs_run = generate_run_id();
    let mut anchor_buf = AlignedBuffer::new(PROBE_BLOCK_SIZE);
    let mut mid_orig = AlignedBuffer::new(PROBE_BLOCK_SIZE);
    let mut mid_buf = AlignedBuffer::new(PROBE_BLOCK_SIZE);
    let mut read_buf = AlignedBuffer::new(PROBE_BLOCK_SIZE);

    for _ in 0..11 {
        if hi.saturating_sub(lo) <= 16 * MIB {
            break;
        }
        let mid = ((lo + (hi - lo) / 2) / MIB) * MIB;
        if mid <= anchor {
            break;
        }

        // Backup mid before touching it
        let mid_backed_up = dev.read_at(mid, &mut mid_orig).is_ok();

        // Write fresh anchor and mid markers
        fill_buffer(&mut anchor_buf, bs_run, anchor);
        fill_buffer(&mut mid_buf, bs_run, mid);
        let _ = dev.write_at(anchor, &anchor_buf);
        let write_ok = dev.write_at(mid, &mid_buf).is_ok();
        let _ = dev.flush_to_media();

        let mid_ok = write_ok
            && dev.read_at(mid, &mut read_buf).is_ok()
            && verify_buffer(&read_buf, bs_run, mid) == VerifyStatus::Ok;
        let anchor_ok = dev.read_at(anchor, &mut read_buf).is_ok()
            && verify_buffer(&read_buf, bs_run, anchor) == VerifyStatus::Ok;

        // Immediately restore mid's original sector!
        if mid_backed_up {
            let _ = dev.write_at(mid, &mid_orig);
            let _ = dev.flush_to_media();
        }

        if mid_ok && anchor_ok {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

/// Generates sorted unique aligned LBA offsets spanning [32 MiB .. disk_bytes - 2 MiB],
/// combining uniform coverage with exact power-of-2 capacity boundaries.
fn build_probe_offsets(disk_bytes: u64, target_count: usize) -> Vec<u64> {
    let start = SAFE_START_OFFSET.min(disk_bytes / 16);
    let end = disk_bytes.saturating_sub(2 * MIB);
    if end <= start + PROBE_BLOCK_SIZE as u64 {
        return vec![start];
    }

    let mut pts = Vec::with_capacity(target_count + 24);
    let span = end - start;
    let n = target_count.max(16);
    for i in 0..n {
        let off = start + (span as u128 * i as u128 / (n - 1) as u128) as u64;
        let aligned = (off / 4096) * 4096;
        pts.push(aligned);
    }

    // Add strategic power-of-two capacity boundary probes (1G, 2G, 4G, 8G, 16G, 32G, 64G, ...)
    let mut gb = 1u64;
    while gb * 1024 * MIB < end {
        let b = gb * 1024 * MIB;
        for delta in [-4 * MIB as i64, 4 * MIB as i64] {
            let candidate = (b as i64 + delta) as u64;
            if candidate > start && candidate < end {
                pts.push((candidate / 4096) * 4096);
            }
        }
        gb *= 2;
    }

    pts.sort_unstable();
    pts.dedup_by(|a, b| a.abs_diff(*b) < (PROBE_BLOCK_SIZE as u64 * 2));
    pts
}

#[cfg(target_os = "macos")]
struct RemountGuard {
    bsd_whole: String,
}

#[cfg(target_os = "macos")]
impl Drop for RemountGuard {
    fn drop(&mut self) {
        let _ = Command::new("diskutil")
            .args(["mountDisk", &self.bsd_whole])
            .output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_offsets_are_aligned_safe_and_cover_boundaries() {
        let size_128gb = 128u64 * 1024 * 1024 * 1024;
        let offsets = build_probe_offsets(size_128gb, 128);
        assert!(offsets.len() >= 128);
        assert!(*offsets.first().unwrap() >= SAFE_START_OFFSET);
        assert!(*offsets.last().unwrap() < size_128gb);
        for w in offsets.windows(2) {
            assert!(w[1] > w[0]);
            assert_eq!(w[0] % 4096, 0);
        }
        // Verify 16 GiB boundary region is probed
        let gb16 = 16u64 * 1024 * 1024 * 1024;
        assert!(offsets.iter().any(|&o| o.abs_diff(gb16) <= 4 * MIB));
    }
}
