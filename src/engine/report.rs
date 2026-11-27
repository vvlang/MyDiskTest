use crate::disk::{format_bytes, format_bytes_decimal, format_duration};
use crate::engine::{LiveTestSnapshot, TestMode, Verdict};

/// Formats a human-readable Chinese Markdown/text diagnostic report.
pub fn format_markdown_report(snap: &LiveTestSnapshot) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("# MyDiskTest 磁盘检测报告\n\n");
    out.push_str(&format!("- **测试模式**: {}\n", snap.config.mode.title_zh()));
    out.push_str(&format!("- **开始时间**: {}\n", snap.started_at));
    if let Some(ref fin) = snap.finished_at {
        out.push_str(&format!("- **结束时间**: {}\n", fin));
    }
    out.push_str(&format!(
        "- **总耗时**: {}\n",
        format_duration(snap.elapsed_secs)
    ));
    out.push_str(&format!(
        "- **无缓冲直写 (Direct I/O)**: {}\n\n",
        if snap.direct_io_active { "已启用" } else { "同步落盘回退" }
    ));

    out.push_str("## 设备硬件信息\n\n");
    out.push_str(&format!("- **卷标 / 名称**: {}\n", snap.disk.name));
    out.push_str(&format!(
        "- **挂载路径**: `{}`\n",
        snap.disk.mount_point.display()
    ));
    out.push_str(&format!("- **设备节点**: `{}`\n", snap.disk.device_id));
    out.push_str(&format!("- **设备类型**: {}\n", snap.disk.kind.label_zh()));
    out.push_str(&format!("- **文件系统**: {}\n", snap.disk.filesystem));
    out.push_str(&format!("- **接口协议**: {}\n", snap.disk.protocol));
    out.push_str(&format!(
        "- **标称总容量**: {} ({})\n",
        format_bytes_decimal(snap.disk.physical_bytes.max(snap.disk.total_bytes)),
        format_bytes(snap.disk.total_bytes)
    ));
    if !snap.disk.vendor.is_empty() {
        out.push_str(&format!("- **厂商**: {}\n", snap.disk.vendor));
    }
    if !snap.disk.model.is_empty() {
        out.push_str(&format!("- **型号**: {}\n", snap.disk.model));
    }
    if let (Some(ref vid), Some(ref pid)) = (&snap.disk.vid, &snap.disk.pid) {
        out.push_str(&format!("- **USB VID / PID**: `{}` / `{}`\n", vid, pid));
    }
    if let Some(ref sn) = snap.disk.serial {
        out.push_str(&format!("- **序列号**: `{}`\n", sn));
    }

    out.push_str("\n## 检测结论\n\n");
    match &snap.verdict {
        Verdict::Healthy { summary, .. } => out.push_str(&format!("> ✅ **正常 / 足容**：{}\n\n", summary)),
        Verdict::FakeCapacity {
            reported_bytes,
            estimated_real_bytes,
            reason,
        } => {
            out.push_str(&format!(
                "> 🚨 **警告：疑似扩容盘 / 假容量设备**\n>\n> - 标称容量：{}\n> - 估算真实容量：{}\n> - 异常原因：{}\n\n",
                format_bytes_decimal(*reported_bytes),
                format_bytes(*estimated_real_bytes),
                reason
            ));
        }
        Verdict::BadBlocks { summary, .. } => {
            out.push_str(&format!("> ⚠️ **存在坏块/数据异常**：{}\n\n", summary));
        }
        Verdict::BenchmarkComplete { summary } => {
            out.push_str(&format!("> 📊 **基准测试完成**：{}\n\n", summary));
        }
        Verdict::AgingComplete { summary, .. } => {
            out.push_str(&format!("> 🔥 **老化拷机结果**：{}\n\n", summary));
        }
        Verdict::Cancelled { summary } => out.push_str(&format!("> ⏹ **已取消**：{}\n\n", summary)),
        Verdict::Failed { error } => out.push_str(&format!("> ❌ **测试失败**：{}\n\n", error)),
        Verdict::Running => out.push_str("> ⏳ 测试进行中\n\n"),
    }

    out.push_str("## 性能与统计指标\n\n");
    out.push_str(&format!(
        "- **平均写入速度**: {:.2} MB/s\n",
        snap.avg_write_mb_s
    ));
    out.push_str(&format!(
        "- **平均读取速度**: {:.2} MB/s\n",
        snap.avg_read_mb_s
    ));
    out.push_str(&format!(
        "- **累计写入数据**: {}\n",
        format_bytes(snap.bytes_written)
    ));
    out.push_str(&format!(
        "- **累计校验数据**: {}\n",
        format_bytes(snap.bytes_verified)
    ));

    if !snap.blocks.is_empty() {
        let (good, slow, bad, wrapped) = snap.count_blocks();
        out.push_str(&format!(
            "- **区块健康分布**: 正常 {} | 慢速 {} | 坏块 {} | 扩容回绕 {}（总计 {} 块）\n",
            good,
            slow,
            bad,
            wrapped,
            snap.blocks.len()
        ));
    }

    if snap.config.mode == TestMode::Benchmark {
        let b = &snap.benchmark;
        out.push_str("\n### 速度基准详情\n\n");
        out.push_str("| 测试项目 | 写入速度 | 读取速度 | 附加指标 |\n");
        out.push_str("| :--- | ---: | ---: | :--- |\n");
        out.push_str(&format!(
            "| 顺序 1MB | {:.2} MB/s | {:.2} MB/s | 写波动 {:.1}~{:.1} / 读波动 {:.1}~{:.1} MB/s |\n",
            b.seq_write_mb_s,
            b.seq_read_mb_s,
            b.seq_write_min_mb_s,
            b.seq_write_max_mb_s,
            b.seq_read_min_mb_s,
            b.seq_read_max_mb_s
        ));
        out.push_str(&format!(
            "| 64KB 块 | {:.2} MB/s | {:.2} MB/s | 中小文件传输 |\n",
            b.block64k_write_mb_s, b.block64k_read_mb_s
        ));
        out.push_str(&format!(
            "| 4K 随机 (QD1) | {:.2} MB/s | {:.2} MB/s | 写 {:.0} IOPS ({:.2}ms) / 读 {:.0} IOPS ({:.2}ms) |\n",
            b.rand4k_write_mb_s,
            b.rand4k_read_mb_s,
            b.rand4k_write_iops,
            b.rand4k_write_lat_ms,
            b.rand4k_read_iops,
            b.rand4k_read_lat_ms
        ));
    }

    if !snap.aging_records.is_empty() {
        out.push_str("\n### 老化拷机轮次明细\n\n");
        out.push_str("| 轮次 | 写入速度 (MB/s) | 读取速度 (MB/s) | 校验错误 | 耗时 |\n");
        out.push_str("| :---: | ---: | ---: | :---: | ---: |\n");
        for r in &snap.aging_records {
            out.push_str(&format!(
                "| #{} | {:.2} | {:.2} | {} | {:.1}s |\n",
                r.cycle, r.write_mb_s, r.read_mb_s, r.errors, r.duration_secs
            ));
        }
    }

    if !snap.bad_sectors.is_empty() {
        out.push_str("\n### 异常扇区 / 坏道明细 (HD Tune Pro 错误扫描)\n\n");
        out.push_str("| 区块 # | 偏移位置 | LBA (512B) | 读取速度 | 类型与说明 |\n");
        out.push_str("| :---: | ---: | ---: | ---: | :--- |\n");
        for entry in snap.bad_sectors.iter().take(200) {
            out.push_str(&format!(
                "| #{} | {} | {} | {:.2} MB/s | {} |\n",
                entry.block_index + 1,
                format_bytes(entry.offset_bytes),
                entry.lba_512,
                entry.speed_mb_s,
                entry.reason
            ));
        }
    }

    out.push_str("\n## 运行日志\n\n```text\n");
    for line in &snap.logs {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("```\n\n---\n*Generated by MyDiskTest Next • Copyright © 2026 AIYeon*\n");
    out
}

pub fn format_json_report(snap: &LiveTestSnapshot) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(snap)
}
