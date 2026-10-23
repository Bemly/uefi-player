use crate::graphics::Screen;
use alloc::string::String;
use core::fmt::Debug;
use core::time::Duration;
use uefi::boot::stall;

pub type Result<Output = (), ErrData = ()> = core::result::Result<Output, NyaStatus<ErrData>>;

#[derive(Debug)]
pub enum NyaStatus<Data: Debug = ()> {
    Uefi(uefi::Error<Data>),
    FromStrWithBufError,
    Qoi(qoi::Error),
    NotRegularFile,
    _Debug(String),
    _Reserve,
}

impl From<uefi::Error> for NyaStatus {
    fn from(e: uefi::Error) -> Self {
        NyaStatus::Uefi(e)
    }
}

// 直接转换状态
impl From<uefi::Status> for NyaStatus {
    fn from(s: uefi::Status) -> Self {
        NyaStatus::Uefi(uefi::Error::new(s, ()))
    }
}

impl From<qoi::Error> for NyaStatus {
    fn from(e: qoi::Error) -> Self {
        NyaStatus::Qoi(e)
    }
}

// 垫底的错误处理，至少是安全的输出内容（大雾
pub fn handle_fatal(err: NyaStatus, screen: &mut Screen) -> ! {
    let _ = screen.clear();

    // 可变参数宏简化
    macro_rules! println {
        ($($arg:tt)*) => { screen.draw_str(&alloc::format!($($arg)*)) }
    }

    println!("KERNEL PANIC!");

    match err {
        NyaStatus::Qoi(err) => println!("QOI error: {}", err),
        NyaStatus::_Debug(err) => screen.draw_str(&err),
        _ => println!("FATAL ERROR: {:?}", err),
    }

    println!("System will stall for 2 minutes before returning.");

    stall(Duration::from_mins(2));

    panic!("Unrecoverable error occurred.");
}
