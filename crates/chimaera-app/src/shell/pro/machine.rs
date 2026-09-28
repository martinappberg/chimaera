//! A display label is not a device identity. Network-assigned hostnames can change.
pub(super) fn display_name() -> String {
    platform_name()
        .as_deref()
        .and_then(valid_name)
        .unwrap_or(if cfg!(target_os = "macos") {
            "My Mac"
        } else {
            "My computer"
        })
        .to_owned()
}
fn valid_name(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 120 && !value.chars().any(char::is_control))
        .then_some(value)
}
#[cfg(target_os = "macos")]
fn platform_name() -> Option<String> {
    use std::ffi::{c_char, c_void, CStr};
    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C" {
        fn SCDynamicStoreCopyComputerName(store: *const c_void, encoding: *mut u32) -> *mut c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(
            value: *mut c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> bool;
        fn CFRelease(value: *mut c_void);
    }
    // A null store asks SystemConfiguration for a temporary local session.
    // Copy returns an owned CFString; release it on every non-null path.
    unsafe {
        let value = SCDynamicStoreCopyComputerName(std::ptr::null(), std::ptr::null_mut());
        if value.is_null() {
            return None;
        }
        let mut buffer = [0 as c_char; 512];
        let copied = CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            0x08000100,
        );
        CFRelease(value);
        copied
            .then(|| {
                CStr::from_ptr(buffer.as_ptr())
                    .to_str()
                    .ok()
                    .map(str::to_owned)
            })
            .flatten()
    }
}
#[cfg(not(target_os = "macos"))]
fn platform_name() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_labels_are_bounded_utf8_without_control_characters() {
        assert_eq!(
            valid_name("  Martin’s MacBook Pro  "),
            Some("Martin’s MacBook Pro")
        );
        for value in ["", "  ", "Mac\nname", "Mac\0name", &"é".repeat(61)] {
            assert!(valid_name(value).is_none());
        }
        assert!(valid_name(&"é".repeat(60)).is_some());
    }
}
