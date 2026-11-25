use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::path::Path;

/// Sector/page alignment required by Windows `FILE_FLAG_NO_BUFFERING` and Linux `O_DIRECT`.
pub const ALIGNMENT: usize = 4096;

/// Heap-allocated buffer guaranteed to be aligned to 4096 bytes.
/// Required for true unbuffered Direct I/O on Windows (`FILE_FLAG_NO_BUFFERING`)
/// and optimal DMA transfers on macOS (`F_NOCACHE`).
pub struct AlignedBuffer {
    ptr: *mut u8,
    len: usize,
    layout: Layout,
}

// Safety: AlignedBuffer exclusively owns its heap allocation like Vec<u8>.
unsafe impl Send for AlignedBuffer {}
unsafe impl Sync for AlignedBuffer {}

impl AlignedBuffer {
    pub fn new(len: usize) -> Self {
        assert!(len > 0, "AlignedBuffer length must be > 0");
        assert!(
            len % ALIGNMENT == 0,
            "AlignedBuffer length ({}) must be a multiple of {}",
            len,
            ALIGNMENT
        );
        let layout = Layout::from_size_align(len, ALIGNMENT).expect("valid aligned layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Self { ptr, len, layout }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Deref for AlignedBuffer {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl DerefMut for AlignedBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.ptr, self.layout);
        }
    }
}

/// Cross-platform Direct/Unbuffered I/O file wrapper.
/// Bypasses OS page cache so fake drives and speed tests cannot be masked by system RAM.
pub struct DirectFile {
    file: File,
    pub unbuffered_active: bool,
}

impl DirectFile {
    /// Creates or truncates a file for unbuffered read+write operations.
    pub fn create_rw(path: &Path) -> io::Result<Self> {
        open_direct_impl(path, true, true, true)
    }

    /// Opens an existing file for unbuffered read+write operations.
    pub fn open_rw(path: &Path) -> io::Result<Self> {
        open_direct_impl(path, true, true, false)
    }

    /// Opens an existing file for unbuffered read-only verification.
    pub fn open_read(path: &Path) -> io::Result<Self> {
        open_direct_impl(path, true, false, false)
    }

    /// Sets file length. On FAT32/exFAT, `set_len` allocates real cluster chains in the FAT table.
    pub fn set_len(&self, size: u64) -> io::Result<()> {
        self.file.set_len(size)
    }

    pub fn seek_to(&mut self, offset: u64) -> io::Result<u64> {
        self.file.seek(SeekFrom::Start(offset))
    }

    pub fn write_all_aligned(&mut self, buf: &[u8]) -> io::Result<()> {
        let mut written = 0;
        while written < buf.len() {
            match self.file.write(&buf[written..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "写入返回 0 字节（可能空间不足或设备断开）",
                    ));
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn read_exact_aligned(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut read_total = 0;
        while read_total < buf.len() {
            match self.file.read(&mut buf[read_total..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "读取意外到达文件末尾（可能为扩容盘或坏块截断）",
                    ));
                }
                Ok(n) => read_total += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.seek_to(offset)?;
        self.write_all_aligned(buf)
    }

    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.seek_to(offset)?;
        self.read_exact_aligned(buf)
    }

    /// Flushes data all the way down to physical flash media.
    pub fn flush_to_media(&mut self) -> io::Result<()> {
        self.file.flush()?;
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = self.file.as_raw_fd();
            // F_FULLFSYNC asks the drive controller to flush its volatile write cache too.
            // Some USB/exFAT drivers return ENOTSUP, in which case sync_data() is the fallback.
            let rc = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) };
            if rc != 0 {
                let _ = self.file.sync_data();
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.file.sync_data()
        }
    }
}

#[cfg(target_os = "macos")]
fn open_direct_impl(
    path: &Path,
    read: bool,
    write: bool,
    create_trunc: bool,
) -> io::Result<DirectFile> {
    use std::os::unix::io::AsRawFd;

    let mut opts = OpenOptions::new();
    opts.read(read).write(write);
    if create_trunc {
        opts.create(true).truncate(true);
    }
    let file = opts.open(path)?;
    let fd = file.as_raw_fd();
    // macOS F_NOCACHE bypasses the Unified Buffer Cache so reads/writes hit physical media.
    let rc = unsafe { libc::fcntl(fd, libc::F_NOCACHE, 1) };
    // Also disable read-ahead so random 4K benchmarks measure true flash latency.
    unsafe {
        libc::fcntl(fd, libc::F_RDAHEAD, 0);
    }
    Ok(DirectFile {
        file,
        unbuffered_active: rc == 0,
    })
}

#[cfg(target_os = "windows")]
fn open_direct_impl(
    path: &Path,
    read: bool,
    write: bool,
    create_trunc: bool,
) -> io::Result<DirectFile> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH,
    };

    let build_opts = |flags: u32| {
        let mut opts = OpenOptions::new();
        opts.read(read).write(write).custom_flags(flags);
        if create_trunc {
            opts.create(true).truncate(true);
        }
        opts
    };

    // Prefer true unbuffered + write-through I/O.
    match build_opts(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH).open(path) {
        Ok(file) => Ok(DirectFile {
            file,
            unbuffered_active: true,
        }),
        Err(_) => {
            // Fallback if a virtual filesystem doesn't allow NO_BUFFERING.
            let file = build_opts(FILE_FLAG_WRITE_THROUGH).open(path)?;
            Ok(DirectFile {
                file,
                unbuffered_active: false,
            })
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_direct_impl(
    path: &Path,
    read: bool,
    write: bool,
    create_trunc: bool,
) -> io::Result<DirectFile> {
    let mut opts = OpenOptions::new();
    opts.read(read).write(write);
    if create_trunc {
        opts.create(true).truncate(true);
    }
    let file = opts.open(path)?;
    Ok(DirectFile {
        file,
        unbuffered_active: false,
    })
}

/// Returns `(total_bytes, available_bytes)` for the filesystem containing `path`.
pub fn get_volume_space(path: &Path) -> io::Result<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径包含非法空字符"))?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let frsize = if stat.f_frsize > 0 {
            stat.f_frsize as u64
        } else {
            stat.f_bsize as u64
        };
        let total = (stat.f_blocks as u64).saturating_mul(frsize);
        let avail = (stat.f_bavail as u64).saturating_mul(frsize);
        Ok((total, avail))
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free_avail: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_avail,
                &mut total_bytes,
                &mut total_free,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((total_bytes, free_avail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_buffer_is_page_aligned() {
        let mut buf = AlignedBuffer::new(ALIGNMENT * 4);
        assert_eq!((buf.as_ptr() as usize) % ALIGNMENT, 0);
        assert_eq!(buf.len(), ALIGNMENT * 4);
        buf[0] = 0xAB;
        buf[ALIGNMENT * 4 - 1] = 0xCD;
        assert_eq!(buf[0], 0xAB);
        assert_eq!(buf[ALIGNMENT * 4 - 1], 0xCD);
    }

    #[test]
    fn direct_file_roundtrip() {
        let dir = std::env::temp_dir().join("mydisktest_unit_direct_io");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.bin");

        let mut wbuf = AlignedBuffer::new(ALIGNMENT * 2);
        for (i, b) in wbuf.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        {
            let mut f = DirectFile::create_rw(&path).expect("create direct");
            f.write_all_aligned(&wbuf).expect("write aligned");
            f.flush_to_media().expect("flush");
        }
        let mut rbuf = AlignedBuffer::new(ALIGNMENT * 2);
        {
            let mut f = DirectFile::open_read(&path).expect("open read direct");
            f.read_exact_aligned(&mut rbuf).expect("read aligned");
        }
        assert_eq!(&*wbuf, &*rbuf);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
