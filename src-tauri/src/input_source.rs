//! Text Input Sources (Carbon/HIToolbox). Every function here must run on the main thread.

use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::boolean::kCFBooleanTrue;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::{CFURL, CFURLRef};
use std::ffi::c_void;
use std::path::{Path, PathBuf};

pub const COLEMAK_NAME: &str = "Colemak DH ANSI";
pub const ABC_ID: &str = "com.apple.keylayout.ABC";
pub const BUNDLE_NAME: &str = "Colemak DH.bundle";

type TISInputSourceRef = *const c_void;
type OSStatus = i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCreateInputSourceList(properties: *const c_void, include_all_installed: u8)
    -> CFArrayRef;
    fn TISGetInputSourceProperty(src: TISInputSourceRef, key: CFStringRef) -> CFTypeRef;
    fn TISRegisterInputSource(location: CFURLRef) -> OSStatus;
    fn TISEnableInputSource(src: TISInputSourceRef) -> OSStatus;
    fn TISSelectInputSource(src: TISInputSourceRef) -> OSStatus;
    fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
    static kTISPropertyLocalizedName: CFStringRef;
    static kTISPropertyInputSourceID: CFStringRef;
    static kTISPropertyInputSourceIsEnabled: CFStringRef;
}

fn layouts_dir() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME not set");
    PathBuf::from(home).join("Library/Keyboard Layouts")
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Copies the bundle into ~/Library/Keyboard Layouts if missing. Returns true if it copied.
pub fn ensure_bundle_installed(resource_dir: &Path) -> Result<bool, String> {
    let dest = layouts_dir().join(BUNDLE_NAME);
    if dest.exists() {
        return Ok(false);
    }
    copy_dir(&resource_dir.join(BUNDLE_NAME), &dest)
        .map_err(|e| format!("Copying {BUNDLE_NAME} failed: {e}"))?;
    let url = CFURL::from_path(&dest, true).ok_or("Bad bundle path")?;
    let status = unsafe { TISRegisterInputSource(url.as_concrete_TypeRef()) };
    if status != 0 {
        return Err(format!("TISRegisterInputSource failed ({status})"));
    }
    Ok(true)
}

fn string_prop(src: TISInputSourceRef, key: CFStringRef) -> Option<String> {
    let v = unsafe { TISGetInputSourceProperty(src, key) };
    (!v.is_null()).then(|| unsafe { CFString::wrap_under_get_rule(v as CFStringRef) }.to_string())
}

/// Finds the first installed source whose `key` property equals `value`, enables and selects it.
fn select_where(key: CFStringRef, value: &str) -> Result<(), String> {
    let filter = CFDictionary::from_CFType_pairs(&[(
        unsafe { CFString::wrap_under_get_rule(key) },
        CFString::new(value),
    )]);
    let list = unsafe { TISCreateInputSourceList(filter.as_concrete_TypeRef().cast(), 1) };
    if list.is_null() {
        return Err(format!("{value} not found"));
    }
    let result = (|| {
        let count = unsafe { CFArrayGetCount(list) };
        let src = (0..count)
            .map(|i| unsafe { CFArrayGetValueAtIndex(list, i) })
            .find(|&s| string_prop(s, key).as_deref() == Some(value))
            .ok_or_else(|| format!("{value} not found"))?;
        // Enabling an already-enabled third-party layout still shows a macOS consent prompt.
        let enabled = unsafe {
            TISGetInputSourceProperty(src, kTISPropertyInputSourceIsEnabled)
                == kCFBooleanTrue as CFTypeRef
        };
        if !enabled {
            let st = unsafe { TISEnableInputSource(src) };
            if st != 0 {
                return Err(format!("Enabling {value} failed ({st})"));
            }
        }
        let st = unsafe { TISSelectInputSource(src) };
        if st != 0 {
            return Err(format!("Selecting {value} failed ({st})"));
        }
        Ok(())
    })();
    unsafe { CFRelease(list as CFTypeRef) };
    result
}

pub fn select_by_name(name: &str) -> Result<(), String> {
    select_where(unsafe { kTISPropertyLocalizedName }, name)
}

pub fn select_by_id(id: &str) -> Result<(), String> {
    select_where(unsafe { kTISPropertyInputSourceID }, id)
}

pub fn current_name() -> Option<String> {
    let src = unsafe { TISCopyCurrentKeyboardInputSource() };
    if src.is_null() {
        return None;
    }
    let name = string_prop(src, unsafe { kTISPropertyLocalizedName });
    unsafe { CFRelease(src) };
    name
}

/// Feature ON: install if needed, then enable + select Colemak DH ANSI.
pub fn enable_colemak(resource_dir: &Path) -> Result<(), String> {
    let copied = ensure_bundle_installed(resource_dir)?;
    select_by_name(COLEMAK_NAME).map_err(|e| {
        if copied || e.ends_with("not found") {
            format!("{COLEMAK_NAME} not found. Log out and back in, then try again.")
        } else {
            e
        }
    })
}
