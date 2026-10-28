use std::ptr::null_mut;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MESSAGEBOX_STYLE,
    MessageBoxW,
};

const TITLE: &str = "KeyReel";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn has_stream(stream: STD_HANDLE) -> bool {
    let handle = unsafe { GetStdHandle(stream) };
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

fn message_box(text: impl AsRef<str>, icon: MESSAGEBOX_STYLE) {
    let text = text.as_ref();
    let text = wide(text);
    let title = wide(TITLE);
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | icon | MB_SETFOREGROUND,
        )
    };
}

pub fn info(text: impl AsRef<str>) {
    let text = text.as_ref();
    if has_stream(STD_OUTPUT_HANDLE) {
        println!("{text}");
    } else {
        message_box(text, MB_ICONINFORMATION);
    }
}

pub fn warn(text: impl AsRef<str>) {
    let text = text.as_ref();
    if has_stream(STD_ERROR_HANDLE) {
        eprintln!("WARNING: {text}");
    } else {
        message_box(text, MB_ICONWARNING);
    }
}

pub fn error(text: impl AsRef<str>) {
    let text = text.as_ref();
    if has_stream(STD_ERROR_HANDLE) {
        eprintln!("{text}");
    } else {
        message_box(text, MB_ICONERROR);
    }
}
