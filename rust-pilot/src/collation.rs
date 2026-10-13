//! ICU4C 78 matches Node 24's collation generation (CLDR 48 / Unicode 17).
//! The pilot requires the host's libicui18n.so.78; it does not use ICU4X data.
use std::{
    cmp::Ordering,
    ffi::{c_char, c_void},
    ptr::NonNull,
};

#[link(name = "libicui18n.so.78", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn ucol_open_78(locale: *const c_char, status: *mut i32) -> *mut c_void;
    fn ucol_close_78(collator: *mut c_void);
    fn ucol_setAttribute_78(collator: *mut c_void, attribute: i32, value: i32, status: *mut i32);
    fn ucol_strcollUTF8_78(
        collator: *const c_void,
        a: *const c_char,
        a_len: i32,
        b: *const c_char,
        b_len: i32,
        status: *mut i32,
    ) -> i32;
}
pub struct Collator(NonNull<c_void>);
impl Default for Collator {
    fn default() -> Self {
        let mut status = 0;
        // ICU accepts a NUL-terminated locale; warnings (negative codes) are success.
        let ptr = unsafe { ucol_open_78(c"en_US".as_ptr(), &mut status) };
        assert!(status <= 0, "ICU open error {status}");
        let this = Self(NonNull::new(ptr).expect("ICU collator"));
        status = 0;
        // Intl.Collator enables canonical normalization: UCOL_NORMALIZATION_MODE=4, UCOL_ON=17.
        unsafe { ucol_setAttribute_78(this.0.as_ptr(), 4, 17, &mut status) };
        assert!(status <= 0, "ICU normalization error {status}");
        this
    }
}
impl Collator {
    pub fn compare(&self, a: &str, b: &str) -> Ordering {
        let mut status = 0;
        // The slices stay live for the call; explicit lengths permit embedded NULs.
        let result = unsafe {
            ucol_strcollUTF8_78(
                self.0.as_ptr(),
                a.as_ptr().cast(),
                a.len().try_into().expect("ICU string length"),
                b.as_ptr().cast(),
                b.len().try_into().expect("ICU string length"),
                &mut status,
            )
        };
        assert!(status <= 0, "ICU compare error {status}");
        result.cmp(&0)
    }
}
impl Drop for Collator {
    fn drop(&mut self) {
        // This value uniquely owns the pointer returned by ucol_open.
        unsafe { ucol_close_78(self.0.as_ptr()) };
    }
}
