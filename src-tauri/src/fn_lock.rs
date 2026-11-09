//! Watches the Logitech keyboard's Fn-lock state over HID++ 2.0 (IOKit HID, own thread + CFRunLoop).
//! Needs the Input Monitoring permission.

use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::sync::Mutex;

const LOGITECH: i32 = 0x46d;
const REPORT_LONG: u8 = 0x11;
const REPORT_LEN: usize = 20;
const DEVICE_INDEX: u8 = 0xFF; // direct (Bluetooth) connection
const READ_SWID: u8 = 0xA;
const LISTEN_EVENT: u32 = 1; // kIOHIDRequestTypeListenEvent
const OUTPUT: u32 = 1; // kIOHIDReportTypeOutput

/// Fn-inversion features, in order of preference: (id, read params, index of the state byte).
const FEATURES: [(u16, &[u8], usize); 3] = [
    (0x40A3, &[0xFF], 1), // multi-host: param = current host, reply = [host, state, default]
    (0x40A2, &[], 0),
    (0x40A0, &[], 0),
];

// ---------- pure protocol ----------

pub fn request(index: u8, function: u8, swid: u8, params: &[u8]) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[..4].copy_from_slice(&[REPORT_LONG, DEVICE_INDEX, index, (function << 4) | swid]);
    r[4..4 + params.len()].copy_from_slice(params);
    r
}

/// Root.getFeature(id) for FEATURES[i]; the swid tells us which one a reply belongs to.
fn get_feature(i: usize) -> [u8; REPORT_LEN] {
    let id = FEATURES[i].0.to_be_bytes();
    request(0, 0, i as u8 + 1, &id)
}

#[derive(Debug, PartialEq)]
pub enum Step {
    Send([u8; REPORT_LEN]),
    /// `media` = top row sends media keys (Fn inverted). `toggled` = device notification, not our read.
    State {
        media: bool,
        toggled: bool,
    },
}

#[derive(Default)]
pub struct Proto {
    /// (feature index on the device, position in FEATURES)
    feature: Option<(u8, usize)>,
}

impl Proto {
    pub fn on_connect(&mut self) -> Step {
        self.feature = None;
        Step::Send(get_feature(0))
    }

    pub fn on_report(&mut self, r: &[u8]) -> Option<Step> {
        if r.len() < 7 || r[0] != REPORT_LONG {
            return None;
        }
        let (index, function, swid) = (r[2], r[3] >> 4, r[3] & 0xF);
        if index == 0 && function == 0 && (1..=FEATURES.len() as u8).contains(&swid) {
            let i = swid as usize - 1;
            return match r[4] {
                0 if i + 1 < FEATURES.len() => Some(Step::Send(get_feature(i + 1))),
                0 => None,
                idx => {
                    self.feature = Some((idx, i));
                    Some(Step::Send(request(idx, 0, READ_SWID, FEATURES[i].1)))
                }
            };
        }
        let (idx, i) = self.feature?;
        if index != idx || function != 0 || !(swid == 0 || swid == READ_SWID) {
            return None;
        }
        Some(Step::State {
            media: r[4 + FEATURES[i].2] & 1 == 1,
            toggled: swid == 0,
        })
    }
}

// ---------- IOKit glue ----------

type IOHIDManagerRef = *mut c_void;
type IOHIDDeviceRef = *mut c_void;
type MatchCb = unsafe extern "C" fn(*mut c_void, i32, *mut c_void, IOHIDDeviceRef);
type ReportCb = unsafe extern "C" fn(*mut c_void, i32, *mut c_void, u32, u32, *mut u8, isize);

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(alloc: *const c_void, options: u32) -> IOHIDManagerRef;
    fn IOHIDManagerSetDeviceMatching(m: IOHIDManagerRef, matching: *const c_void);
    fn IOHIDManagerRegisterDeviceMatchingCallback(
        m: IOHIDManagerRef,
        cb: MatchCb,
        ctx: *mut c_void,
    );
    fn IOHIDManagerRegisterInputReportCallback(m: IOHIDManagerRef, cb: ReportCb, ctx: *mut c_void);
    fn IOHIDManagerScheduleWithRunLoop(m: IOHIDManagerRef, rl: *const c_void, mode: CFStringRef);
    fn IOHIDManagerOpen(m: IOHIDManagerRef, options: u32) -> i32;
    fn IOHIDDeviceSetReport(d: IOHIDDeviceRef, t: u32, id: isize, r: *const u8, len: isize) -> i32;
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}

type ChangeFn = dyn Fn(bool, bool) + Send + Sync;

struct Ctx {
    proto: Mutex<Proto>,
    on_change: Box<ChangeFn>,
}

impl Ctx {
    fn run(&self, device: IOHIDDeviceRef, step: Option<Step>) {
        match step {
            Some(Step::Send(r)) => unsafe {
                let rc = IOHIDDeviceSetReport(
                    device,
                    OUTPUT,
                    REPORT_LONG as isize,
                    r.as_ptr(),
                    r.len() as isize,
                );
                if rc != 0 {
                    eprintln!("fn-lock: send failed {rc:#x}");
                }
            },
            Some(Step::State { media, toggled }) => (self.on_change)(media, toggled),
            None => {}
        }
    }
}

unsafe extern "C" fn on_match(ctx: *mut c_void, _: i32, _: *mut c_void, device: IOHIDDeviceRef) {
    let ctx = unsafe { &*(ctx as *const Ctx) };
    let step = ctx
        .proto
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .on_connect();
    ctx.run(device, Some(step));
}

unsafe extern "C" fn on_report(
    ctx: *mut c_void,
    _: i32,
    device: *mut c_void,
    _: u32,
    id: u32,
    report: *mut u8,
    len: isize,
) {
    if id != REPORT_LONG as u32 || report.is_null() || len <= 0 {
        return;
    }
    let ctx = unsafe { &*(ctx as *const Ctx) };
    let r = unsafe { std::slice::from_raw_parts(report, len as usize) };
    let step = ctx
        .proto
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .on_report(r);
    ctx.run(device, step);
}

/// Starts watching. `on_change(media_keys, toggled_by_user)` runs on the watcher thread.
pub fn start(on_change: impl Fn(bool, bool) + Send + Sync + 'static) {
    let ctx = Ctx {
        proto: Mutex::default(),
        on_change: Box::new(on_change),
    };
    let _ = std::thread::Builder::new()
        .name("fn-lock".into())
        .spawn(move || {
            unsafe {
                if IOHIDCheckAccess(LISTEN_EVENT) != 0 && !IOHIDRequestAccess(LISTEN_EVENT) {
                    eprintln!("fn-lock: needs Input Monitoring permission");
                    return;
                }
            }
            // Lives as long as the thread's run loop, i.e. the process.
            let ctx: &'static Ctx = Box::leak(Box::new(ctx));
            let matching = CFDictionary::from_CFType_pairs(&[
                (CFString::new("VendorID"), CFNumber::from(LOGITECH)),
                (CFString::new("PrimaryUsagePage"), CFNumber::from(1)),
                (CFString::new("PrimaryUsage"), CFNumber::from(6)), // keyboard
            ]);
            unsafe {
                let m = IOHIDManagerCreate(std::ptr::null(), 0);
                let p = ctx as *const Ctx as *mut c_void;
                IOHIDManagerSetDeviceMatching(m, matching.as_concrete_TypeRef().cast());
                IOHIDManagerRegisterDeviceMatchingCallback(m, on_match, p);
                IOHIDManagerRegisterInputReportCallback(m, on_report, p);
                IOHIDManagerScheduleWithRunLoop(
                    m,
                    CFRunLoop::get_current().as_concrete_TypeRef().cast(),
                    kCFRunLoopDefaultMode,
                );
                let rc = IOHIDManagerOpen(m, 0);
                if rc != 0 {
                    eprintln!("fn-lock: open failed {rc:#x}");
                }
            }
            CFRunLoop::run_current();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(index: u8, function: u8, swid: u8, params: &[u8]) -> [u8; REPORT_LEN] {
        request(index, function, swid, params)
    }

    #[test]
    fn discovers_multi_host_feature_and_reads_state() {
        let mut p = Proto::default();
        assert_eq!(p.on_connect(), Step::Send(request(0, 0, 1, &[0x40, 0xA3])));
        // 0x40A3 lives at index 9 → read it for the current host.
        assert_eq!(
            p.on_report(&reply(0, 0, 1, &[9])),
            Some(Step::Send(request(9, 0, READ_SWID, &[0xFF])))
        );
        assert_eq!(
            p.on_report(&reply(9, 0, READ_SWID, &[0, 1, 0])),
            Some(Step::State {
                media: true,
                toggled: false
            })
        );
        // Device notification after Fn+Esc.
        assert_eq!(
            p.on_report(&reply(9, 0, 0, &[0, 0])),
            Some(Step::State {
                media: false,
                toggled: true
            })
        );
        // Other features / keys are ignored.
        assert_eq!(p.on_report(&reply(5, 0, 0, &[1])), None);
        assert_eq!(p.on_report(&[0x01, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn falls_back_to_older_features() {
        let mut p = Proto::default();
        p.on_connect();
        assert_eq!(
            p.on_report(&reply(0, 0, 1, &[0])),
            Some(Step::Send(request(0, 0, 2, &[0x40, 0xA2])))
        );
        assert_eq!(
            p.on_report(&reply(0, 0, 2, &[0])),
            Some(Step::Send(request(0, 0, 3, &[0x40, 0xA0])))
        );
        assert_eq!(
            p.on_report(&reply(0, 0, 3, &[4])),
            Some(Step::Send(request(4, 0, READ_SWID, &[])))
        );
        assert_eq!(
            p.on_report(&reply(4, 0, 0, &[1])),
            Some(Step::State {
                media: true,
                toggled: true
            })
        );
        let mut q = Proto::default();
        q.on_connect();
        for swid in 1..=3 {
            q.on_report(&reply(0, 0, swid, &[0]));
        }
        assert_eq!(q.on_report(&reply(4, 0, 0, &[1])), None);
    }
}
