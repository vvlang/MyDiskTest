# MyDiskTest Next

> 专为最新 **Windows 11 (x64 / ARM64)** 与 **macOS (Apple Silicon / Intel)** 打造的跨平台原生存储扩容检测、坏块扫描与性能基准测试工具——经典 MyDiskTest 的现代化重制与增强版。

## ✨ 核心功能

1. **⚡ 快速扩容测试（经典招牌功能）**
   - **极速底层 LBA 探针模式**：跨全盘均匀区间与所有 $2^N$ 容量边界写入极小指纹探针并回读校验，测前备份原扇区、测后逆序无损还原并自动重挂载，128GB~2TB 均只需约 **5~15 秒**；检出扩容时自动执行二分搜索精确定位真实闪存容量。
   - **安全文件模式**：免管理员权限，自动规避内核写零与稀疏文件陷阱。

2. **🛡 全盘完整性校验 & 坏块扫描**
   - 自动按 FAT32 安全的 1 GiB 分片写入带唯一批次 ID、全局逻辑偏移和 64 位校验和的非压缩随机数据，随后绕过系统缓存逐扇区回读比对。
   - 经典 **坏块与容量分布网格图**：实时以 🟢正常 / 🔵待校 / 🟡慢速预警 / 🔴坏块 / 🟣扩容回绕 着色展示，鼠标悬停可查看每个区块的精确字节偏移与读写速率。

3. **🔍 错误扫描（HD Tune Pro 经典模式）**
   - **100% 纯只读零写入**，无需卸载磁盘，直接支持系统盘、满盘、只读盘、外置 HDD / SSD / U 盘 / SD 卡。
   - 支持 **快速扫描 (Quick Scan)** 与 **完整表面扫描**、自定义起止 GB 区间、400 / 1000（经典 50×20）/ 2500 格高密度方格图、实时损坏率统计、精确 **LBA 坏道坐标清单** 与全盘表面读取曲线。

4. **🚀 读写速度基准测试**
   - **顺序 1MB 读写**（附带实时速度波动曲线，一眼看穿 SLC 缓外掉速）
   - **64KB 中小块读写**（贴近日常照片/文档传输）
   - **4K 随机读写 (QD1)**（输出 MB/s、IOPS 与平均毫秒延迟）

5. **🔥 老化 / 拷机测试**
   - 支持自定义轮次（3 / 5 / 10 / 20 轮或无限循环），持续无缓冲读写校验，自动统计每轮错误数与发热掉速百分比。

6. **🔍 设备硬件识别 & 报告导出**
   - 自动识别 U 盘 / SD 卡 / 移动硬盘 / 内置盘，读取文件系统、接口协议、厂商型号、USB **VID / PID**、连接速率和序列号。
   - 内置**系统启动盘默认锁定保护**与**安全文件模式**（绝不破坏分区表与原有文件，测试结束或中途取消自动清理临时文件）。
   - 一键导出 **Markdown (.md)** 或 **JSON (.json)** 完整检测报告。

## 🔧 底层防作弊（Direct I/O）原理

- **macOS**：启用 `fcntl(fd, F_NOCACHE, 1)` + `F_RDAHEAD=0` + `F_FULLFSYNC`，绕过系统 Unified Buffer Cache 直达物理介质。
- **Windows**：启用 `FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH` 并使用 4096 字节页对齐堆缓冲区（`AlignedBuffer`），彻底杜绝系统内存缓存掩盖扩容或虚标测速。

## 🚀 本地编译与打包

前置条件：安装 [Rust 工具链](https://rustup.rs/)（Edition 2021）。

### macOS（Apple Silicon & Intel）

```bash
# 直接运行
cargo run --release

# 打包为原生 MyDiskTest.app
chmod +x scripts/build-macos.sh
./scripts/build-macos.sh
open dist/MyDiskTest.app
```

### Windows（Windows 10 / 11）

```powershell
# 直接运行
cargo run --release

# 打包为绿色单文件 MyDiskTest-Windows.exe
.\scripts\build-windows.ps1
```
