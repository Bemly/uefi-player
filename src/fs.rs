use crate::error::{NyaStatus, Result};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use uefi::boot::{get_image_file_system, image_handle};
use uefi::proto::media::file::{Directory, File, FileAttribute, FileInfo, FileMode, RegularFile};
use uefi::{CStr16, Handle, Status};

pub struct Fs {
    pub root_dir: Directory,
}

impl Fs {
    pub fn new() -> Result<Self> {
        Ok(Self {
            root_dir: get_image_file_system(image_handle())?.open_volume()?,
        })
    }

    /// 打开任意卷（多卷自动发现用；`None` 即本卷，同 `new`）。
    /// 卷句柄来自 `find_handles::<SimpleFileSystem>`，打开失败调用方跳过该卷。
    pub fn open(handle: Option<Handle>) -> Result<Self> {
        match handle {
            None => Self::new(),
            Some(h) => {
                use uefi::proto::media::fs::SimpleFileSystem;
                let mut sfs = uefi::boot::open_protocol_exclusive::<SimpleFileSystem>(h)?;
                Ok(Self {
                    root_dir: sfs.open_volume()?,
                })
            }
        }
    }

    // 没有方法重载 没有可选参数 没有默认形参值！草了
    // <- 给古老的我:对于编译时确定的值可以用宏($($arg:tt)*)来实现可变参数
    // <- 给过去的我:骗你的，format_args是硬编码
    #[inline]
    pub fn open_file(&mut self, path: &CStr16) -> Result<RegularFile> {
        self.open_file_mode(path, FileMode::Read)
    }

    #[inline]
    pub fn open_file_mode(&mut self, path: &CStr16, mode: FileMode) -> Result<RegularFile> {
        self.open_file_attr(path, mode, FileAttribute::empty())
    }

    #[inline]
    pub fn open_file_attr(
        &mut self,
        path: &CStr16,
        mode: FileMode,
        attr: FileAttribute,
    ) -> Result<RegularFile> {
        self.root_dir
            .open(path, mode, attr)?
            .into_regular_file()
            .ok_or(NyaStatus::NotRegularFile)
    }

    /// 创建（或打开已存在）可写文件。
    #[inline]
    pub fn create_file(&mut self, path: &CStr16) -> Result<RegularFile> {
        self.root_dir
            .open(path, FileMode::CreateReadWrite, FileAttribute::empty())?
            .into_regular_file()
            .ok_or(NyaStatus::NotRegularFile)
    }

    /// 一次性写文件。UEFI 没有 truncate 语义：已存在就先删再建，
    /// 保证旧长文件残留字节不会污染本次内容。
    /// 注意：必须用可写模式打开才能 Delete，读模式句柄删会被固件拒绝
    /// （失败静默则旧文件残留，曾实测出文件尾多一行旧内容）。
    pub fn write_file(&mut self, path: &CStr16, data: &[u8]) -> Result {
        if let Ok(old) = self.open_file_attr(path, FileMode::ReadWrite, FileAttribute::empty()) {
            let _ = old.delete();
        }
        let mut file = self.create_file(path)?;
        file.write(data).map_err(|e| e.status())?;
        file.flush()?;
        Ok(())
    }

    /// 打开子目录（BLS `loader/entries`、壁纸 `wallpaper/` 枚举用）。
    pub fn open_dir(&mut self, path: &CStr16) -> Result<Directory> {
        self.root_dir
            .open(path, FileMode::Read, FileAttribute::empty())?
            .into_directory()
            .ok_or(NyaStatus::NotRegularFile)
    }

    /// 列目录直属项 `(名, 是否目录)`。`uefi-rs` 没有现成 walker，
    /// `read_entry` 循环 20 行自研（见 `src/boot/entries.rs` 头注释轮子结论）。
    /// 固定 512B 缓冲：FAT 长文件名一般够用，超长返回 BUFFER_TOO_SMALL 即停
    /// （调用方记 warning，不炸；BLS/壁纸文件名都是短名）。
    pub fn list_dir(&mut self, path: &CStr16) -> Result<Vec<(String, bool)>> {
        let mut dir = self.open_dir(path)?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 512];
        loop {
            match dir.read_entry(&mut buf) {
                Ok(None) => break,
                Ok(Some(info)) => {
                    let name = String::from(info.file_name());
                    if name == "." || name == ".." {
                        continue;
                    }
                    out.push((name, info.is_directory()));
                }
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => break,
                Err(e) => Err(e.status())?,
            }
        }
        Ok(out)
    }

    // 一次性读取全部内容，慎用
    pub fn read_file(&mut self, path: &CStr16) -> Result<Vec<u8>> {
        let mut file = self.open_file(path)?;

        // 获取文件信息
        let mut info_buf = vec![0u8; 128];
        let info = loop {
            match file.get_info::<FileInfo>(&mut info_buf) {
                Ok(info) => break info,
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => {
                    if let Some(size) = *e.data() {
                        info_buf.resize(size, 0)
                    }
                }
                // 其他状态的Option均为None,这里拉平
                Err(e) => Err(e.status())?,
            }
        };

        // 获取完整内容
        let mut file_content = vec![0u8; info.file_size() as usize];
        file.read(&mut file_content)?;

        Ok(file_content)
    }

    /// 流式读一帧（`u32le len + QOI`，旧 `stream` 管线边读边解用）。
    /// 固件短读是常态：头和体都循环补齐，读满才算一帧；尾部不足直接结束，不 panic。
    pub fn read_frame_next(&mut self, file: &mut RegularFile, buf: &mut Vec<u8>) -> Result<bool> {
        // 读头
        let mut qoi_size = [0u8; 4];
        let mut got = 0;
        while got < 4 {
            let n = file.read(&mut qoi_size[got..])?;
            if n == 0 {
                return Ok(false);
            }
            got += n;
        }
        let qoi_size = u32::from_le_bytes(qoi_size) as usize;

        // 扩容缓冲区
        if buf.len() < qoi_size {
            buf.resize(qoi_size, 0);
        }

        // 读满一帧
        let mut done = 0;
        while done < qoi_size {
            let n = file.read(&mut buf[done..qoi_size])?;
            if n == 0 {
                return Ok(false);
            }
            done += n;
        }

        Ok(true)
    }
}
