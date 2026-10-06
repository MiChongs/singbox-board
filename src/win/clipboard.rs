//! The Windows clipboard, for terminals that ignore OSC 52 (conhost).

use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

/// Puts `text` on the clipboard; `false` when it is held by someone else.
pub fn set_text(text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the memory block is sized for `wide` and handed to the
    // clipboard, which owns it once SetClipboardData succeeds.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        let mut done = false;
        if EmptyClipboard() != 0 {
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            if !memory.is_null() {
                let target = GlobalLock(memory).cast::<u16>();
                if !target.is_null() {
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
                    GlobalUnlock(memory);
                    done = !SetClipboardData(u32::from(CF_UNICODETEXT), memory).is_null();
                }
                if !done {
                    GlobalFree(memory);
                }
            }
        }
        CloseClipboard();
        done
    }
}
