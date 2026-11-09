//! Lock key → sleep and scroll speed, via a CGEventTap running on its own thread + CFRunLoop.

use crate::config::LockTrigger;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::mach_port::{CFMachPort, CFMachPortRef};
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{CGEventFlags, EventField};
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};

const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const NX_SYSDEFINED: u32 = 14;
const SCROLL_WHEEL: u32 = 22;
// kCGScrollWheelEvent{Delta,FixedPtDelta,PointDelta}Axis{1,2}
const SCROLL_INT_FIELDS: [u32; 4] = [11, 12, 96, 97];
const SCROLL_FIXED_FIELDS: [u32; 2] = [93, 94];
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
const AUX_CONTROL_BUTTONS: i16 = 8;

pub const ACCESSIBILITY_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

type CGEventRef = *mut c_void;
type TapCallback = unsafe extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventGetDoubleValueField(event: CGEventRef, field: u32) -> f64;
    fn CGEventSetDoubleValueField(event: CGEventRef, field: u32, value: f64);
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

// ---------- pure logic ----------

pub fn mask_flags(flags: u64) -> u64 {
    (CGEventFlags::CGEventFlagShift
        | CGEventFlags::CGEventFlagControl
        | CGEventFlags::CGEventFlagAlternate
        | CGEventFlags::CGEventFlagCommand
        | CGEventFlags::CGEventFlagSecondaryFn)
        .bits()
        & flags
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    KeyDown,
    KeyUp,
    FlagsChanged,
    System,
}

/// Plain description of one tapped event.
#[derive(Debug, Clone, Copy)]
pub struct EventSummary {
    pub kind: Kind,
    pub keycode: u16,
    pub flags: u64,
    pub autorepeat: bool,
    pub subtype: i16,
    pub data1: i64,
}

impl EventSummary {
    pub fn data1_keycode(&self) -> u32 {
        ((self.data1 as u64 & 0xFFFF_0000) >> 16) as u32
    }
    fn system_down(&self) -> bool {
        (self.data1 & 0xFF00) >> 8 == 0x0A
    }
    fn system_repeat(&self) -> bool {
        self.data1 & 0x1 == 1
    }
    /// Key-down that isn't an autorepeat.
    pub fn is_press(&self) -> bool {
        match self.kind {
            Kind::KeyDown => !self.autorepeat,
            Kind::System => {
                self.subtype == AUX_CONTROL_BUTTONS && self.system_down() && !self.system_repeat()
            }
            _ => false,
        }
    }
    /// What this event would be learned as (only for presses).
    pub fn as_trigger(&self) -> Option<LockTrigger> {
        if !self.is_press() {
            return None;
        }
        Some(match self.kind {
            Kind::KeyDown => LockTrigger::Key {
                keycode: self.keycode,
                flags: mask_flags(self.flags),
            },
            _ => LockTrigger::System {
                subtype: self.subtype,
                data1_keycode: self.data1_keycode(),
            },
        })
    }
}

/// Matches key-down/up and system events (any state) against the trigger.
pub fn matches(trigger: &LockTrigger, ev: &EventSummary) -> bool {
    match *trigger {
        LockTrigger::Key { keycode, flags } => {
            matches!(ev.kind, Kind::KeyDown | Kind::KeyUp)
                && ev.keycode == keycode
                && mask_flags(ev.flags) == flags
        }
        LockTrigger::System {
            subtype,
            data1_keycode,
        } => {
            ev.kind == Kind::System && ev.subtype == subtype && ev.data1_keycode() == data1_keycode
        }
    }
}

/// Scales an integer scroll delta, keeping at least one unit so slow speeds still scroll.
pub fn scale_delta(v: i64, speed: f64) -> i64 {
    if v == 0 {
        return 0;
    }
    match (v as f64 * speed).round() as i64 {
        0 => v.signum(),
        r => r,
    }
}

pub fn describe(trigger: &LockTrigger) -> String {
    match *trigger {
        LockTrigger::Key { keycode, flags } => {
            let mods: String = [
                (CGEventFlags::CGEventFlagControl, "⌃"),
                (CGEventFlags::CGEventFlagAlternate, "⌥"),
                (CGEventFlags::CGEventFlagShift, "⇧"),
                (CGEventFlags::CGEventFlagCommand, "⌘"),
                (CGEventFlags::CGEventFlagSecondaryFn, "fn"),
            ]
            .iter()
            .filter(|(f, _)| flags & f.bits() != 0)
            .map(|(_, s)| *s)
            .collect();
            if mods.is_empty() {
                format!("keycode {keycode}")
            } else {
                format!("keycode {keycode} + {mods}")
            }
        }
        LockTrigger::System { data1_keycode, .. } => format!("system key {data1_keycode}"),
    }
}

/// What the tap should do with one event, and the state changes that go with it.
#[derive(Debug, PartialEq)]
pub enum Action {
    Pass,
    Swallow,
    SwallowAndSleep,
    Learned(LockTrigger),
}

pub struct TapState {
    pub enabled: bool,
    pub trigger: Option<LockTrigger>,
    pub learning: bool,
    pub running: bool,
    pub scroll_speed: f64,
    /// Keycode whose key-up we still owe a swallow.
    swallow_up: Option<u16>,
}

impl Default for TapState {
    fn default() -> Self {
        TapState {
            enabled: false,
            trigger: None,
            learning: false,
            running: false,
            scroll_speed: 1.0,
            swallow_up: None,
        }
    }
}

impl TapState {
    pub fn handle(&mut self, ev: &EventSummary) -> Action {
        if ev.kind == Kind::KeyUp && self.swallow_up == Some(ev.keycode) {
            self.swallow_up = None;
            return Action::Swallow;
        }
        if self.learning {
            if let Some(t) = ev.as_trigger() {
                self.learning = false;
                if ev.kind == Kind::KeyDown {
                    self.swallow_up = Some(ev.keycode);
                }
                self.trigger = Some(t.clone());
                return Action::Learned(t);
            }
            return Action::Pass;
        }
        match &self.trigger {
            Some(t) if self.enabled && matches(t, ev) => {
                if ev.kind == Kind::KeyDown {
                    self.swallow_up = Some(ev.keycode);
                }
                if ev.is_press() {
                    Action::SwallowAndSleep
                } else {
                    Action::Swallow
                }
            }
            _ => Action::Pass,
        }
    }
}

// ---------- system glue ----------

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

fn accessibility_prompt() -> bool {
    let opts = CFDictionary::from_CFType_pairs(&[(
        unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) },
        CFBoolean::true_value(),
    )]);
    unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef().cast()) != 0 }
}

/// NSEvent subtype + data1 for an NX_SYSDEFINED CGEvent.
fn system_fields(event: CGEventRef) -> (i16, i64) {
    unsafe {
        let pool = objc_autoreleasePoolPush();
        let send_obj: unsafe extern "C" fn(*mut c_void, *mut c_void, CGEventRef) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let send_i16: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i16 =
            std::mem::transmute(objc_msgSend as *const ());
        let send_i64: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i64 =
            std::mem::transmute(objc_msgSend as *const ());
        let cls = objc_getClass(c"NSEvent".as_ptr());
        let ns = send_obj(cls, sel_registerName(c"eventWithCGEvent:".as_ptr()), event);
        let out = if ns.is_null() {
            (0, 0)
        } else {
            (
                send_i16(ns, sel_registerName(c"subtype".as_ptr())),
                send_i64(ns, sel_registerName(c"data1".as_ptr())),
            )
        };
        objc_autoreleasePoolPop(pool);
        out
    }
}

fn scale_scroll(event: CGEventRef, speed: f64) {
    unsafe {
        // Read everything first: setting the line delta can recompute the others.
        let ints = SCROLL_INT_FIELDS.map(|f| CGEventGetIntegerValueField(event, f));
        let fixed = SCROLL_FIXED_FIELDS.map(|f| CGEventGetDoubleValueField(event, f));
        for (f, v) in SCROLL_INT_FIELDS.iter().zip(ints) {
            CGEventSetIntegerValueField(event, *f, scale_delta(v, speed));
        }
        for (f, v) in SCROLL_FIXED_FIELDS.iter().zip(fixed) {
            CGEventSetDoubleValueField(event, *f, v * speed);
        }
    }
}

fn summarize(etype: u32, event: CGEventRef) -> Option<EventSummary> {
    let kind = match etype {
        KEY_DOWN => Kind::KeyDown,
        KEY_UP => Kind::KeyUp,
        FLAGS_CHANGED => Kind::FlagsChanged,
        NX_SYSDEFINED => Kind::System,
        _ => return None,
    };
    let field = |f| unsafe { CGEventGetIntegerValueField(event, f) };
    let (subtype, data1) = if kind == Kind::System {
        system_fields(event)
    } else {
        (0, 0)
    };
    Some(EventSummary {
        kind,
        keycode: field(EventField::KEYBOARD_EVENT_KEYCODE) as u16,
        flags: unsafe { CGEventGetFlags(event) },
        autorepeat: field(EventField::KEYBOARD_EVENT_AUTOREPEAT) != 0,
        subtype,
        data1,
    })
}

type LearnedFn = dyn Fn(LockTrigger) + Send + Sync;

struct Ctx {
    state: Arc<Mutex<TapState>>,
    on_learned: Arc<LearnedFn>,
    port: AtomicPtr<c_void>,
}

unsafe extern "C" fn tap_callback(
    _proxy: *mut c_void,
    etype: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let ctx = unsafe { &*(user_info as *const Ctx) };
    if etype == TAP_DISABLED_BY_TIMEOUT || etype == TAP_DISABLED_BY_USER_INPUT {
        let port = ctx.port.load(Ordering::Acquire);
        if !port.is_null() {
            unsafe { CGEventTapEnable(port.cast(), true) };
        }
        return event;
    }
    if etype == SCROLL_WHEEL {
        let speed = lock(&ctx.state).scroll_speed;
        if speed != 1.0 {
            scale_scroll(event, speed);
        }
        return event;
    }
    let Some(ev) = summarize(etype, event) else {
        return event;
    };
    let action = lock(&ctx.state).handle(&ev);
    match action {
        Action::Pass => event,
        Action::Swallow => std::ptr::null_mut(),
        Action::SwallowAndSleep => {
            std::thread::spawn(|| {
                let _ = std::process::Command::new("/usr/bin/pmset")
                    .arg("sleepnow")
                    .status();
            });
            std::ptr::null_mut()
        }
        Action::Learned(t) => {
            let f = ctx.on_learned.clone();
            std::thread::spawn(move || f(t));
            std::ptr::null_mut()
        }
    }
}

fn lock(m: &Mutex<TapState>) -> MutexGuard<'_, TapState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct LockTap {
    state: Arc<Mutex<TapState>>,
    on_learned: Arc<LearnedFn>,
}

impl LockTap {
    pub fn new(on_learned: impl Fn(LockTrigger) + Send + Sync + 'static) -> Self {
        LockTap {
            state: Arc::default(),
            on_learned: Arc::new(on_learned),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, TapState> {
        lock(&self.state)
    }

    /// Starts the tap thread once. Prompts for Accessibility if needed.
    pub fn ensure_running(&self) -> Result<(), String> {
        if self.state().running {
            return Ok(());
        }
        if !accessibility_prompt() {
            return Err("Needs Accessibility permission".into());
        }
        let ctx = Ctx {
            state: self.state.clone(),
            on_learned: self.on_learned.clone(),
            port: AtomicPtr::default(),
        };
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("lock-tap".into())
            .spawn(move || {
                // Lives as long as the thread's run loop, i.e. the process.
                let ctx: &'static Ctx = Box::leak(Box::new(ctx));
                let mask = [KEY_DOWN, KEY_UP, FLAGS_CHANGED, NX_SYSDEFINED, SCROLL_WHEEL]
                    .iter()
                    .fold(0u64, |m, t| m | (1 << t));
                let raw = unsafe {
                    CGEventTapCreate(
                        1, // kCGSessionEventTap
                        0, // kCGHeadInsertEventTap
                        0, // kCGEventTapOptionDefault
                        mask,
                        tap_callback,
                        ctx as *const Ctx as *mut c_void,
                    )
                };
                if raw.is_null() {
                    let _ = tx.send(Err("Could not create event tap".to_string()));
                    return;
                }
                ctx.port.store(raw.cast(), Ordering::Release);
                let port = unsafe { CFMachPort::wrap_under_create_rule(raw) };
                let Ok(source) = port.create_runloop_source(0) else {
                    let _ = tx.send(Err("Could not create run loop source".to_string()));
                    return;
                };
                CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
                unsafe { CGEventTapEnable(raw, true) };
                let _ = tx.send(Ok(()));
                CFRunLoop::run_current();
            })
            .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())??;
        self.state().running = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_CMD: u64 = 0x40000 | 0x100000; // 1310720

    fn key(kind: Kind, keycode: u16, flags: u64) -> EventSummary {
        EventSummary {
            kind,
            keycode,
            flags,
            autorepeat: false,
            subtype: 0,
            data1: 0,
        }
    }

    fn sys(code: u32, down: bool) -> EventSummary {
        let state = if down { 0x0A } else { 0x0B };
        EventSummary {
            kind: Kind::System,
            keycode: 0,
            flags: 0,
            autorepeat: false,
            subtype: 8,
            data1: ((code as i64) << 16) | (state << 8),
        }
    }

    #[test]
    fn flag_masking() {
        // Caps lock (0x10000), non-coalesced (0x100) and device bits are dropped.
        assert_eq!(mask_flags(CTRL_CMD | 0x10000 | 0x100 | 0x9), CTRL_CMD);
        assert_eq!(mask_flags(0x20000 | 0x80000 | 0x800000), 0x8A0000);
        assert_eq!(mask_flags(0xFF00FFFF), 0);
    }

    #[test]
    fn key_trigger_matching() {
        let t = LockTrigger::Key {
            keycode: 12,
            flags: CTRL_CMD,
        };
        assert!(matches(&t, &key(Kind::KeyDown, 12, CTRL_CMD | 0x100)));
        assert!(matches(&t, &key(Kind::KeyUp, 12, CTRL_CMD)));
        assert!(!matches(&t, &key(Kind::KeyDown, 12, 0x100000)));
        assert!(!matches(&t, &key(Kind::KeyDown, 13, CTRL_CMD)));
        assert!(!matches(&t, &key(Kind::FlagsChanged, 12, CTRL_CMD)));
        assert!(!matches(&t, &sys(12, true)));
    }

    #[test]
    fn system_trigger_matching() {
        let t = LockTrigger::System {
            subtype: 8,
            data1_keycode: 6,
        };
        assert!(matches(&t, &sys(6, true)));
        assert!(matches(&t, &sys(6, false)));
        assert!(!matches(&t, &sys(7, true)));
        let mut other = sys(6, true);
        other.subtype = 7;
        assert!(!matches(&t, &other));
        assert!(!matches(&t, &key(Kind::KeyDown, 6, 0)));
    }

    #[test]
    fn learn_then_sleep_flow() {
        let mut s = TapState {
            learning: true,
            ..Default::default()
        };
        // Modifier presses are not learned.
        assert_eq!(
            s.handle(&key(Kind::FlagsChanged, 59, 0x40000)),
            Action::Pass
        );
        let learned = LockTrigger::Key {
            keycode: 12,
            flags: CTRL_CMD,
        };
        assert_eq!(
            s.handle(&key(Kind::KeyDown, 12, CTRL_CMD)),
            Action::Learned(learned.clone())
        );
        assert!(!s.learning);
        assert_eq!(s.handle(&key(Kind::KeyUp, 12, 0)), Action::Swallow);

        // Disabled: passes through.
        assert_eq!(s.handle(&key(Kind::KeyDown, 12, CTRL_CMD)), Action::Pass);
        assert_eq!(s.handle(&key(Kind::KeyUp, 12, CTRL_CMD)), Action::Pass);

        s.enabled = true;
        assert_eq!(
            s.handle(&key(Kind::KeyDown, 12, CTRL_CMD)),
            Action::SwallowAndSleep
        );
        let mut repeat = key(Kind::KeyDown, 12, CTRL_CMD);
        repeat.autorepeat = true;
        assert_eq!(s.handle(&repeat), Action::Swallow);
        // Key-up swallowed even if modifiers were released first.
        assert_eq!(s.handle(&key(Kind::KeyUp, 12, 0)), Action::Swallow);
        assert_eq!(s.handle(&key(Kind::KeyDown, 0, 0)), Action::Pass);
    }

    #[test]
    fn system_learn_and_match() {
        let mut s = TapState {
            learning: true,
            ..Default::default()
        };
        // Key-up of a system key is not learned.
        assert_eq!(s.handle(&sys(6, false)), Action::Pass);
        let t = LockTrigger::System {
            subtype: 8,
            data1_keycode: 6,
        };
        assert_eq!(s.handle(&sys(6, true)), Action::Learned(t.clone()));
        s.enabled = true;
        assert_eq!(s.handle(&sys(6, true)), Action::SwallowAndSleep);
        assert_eq!(s.handle(&sys(6, false)), Action::Swallow);
        assert_eq!(s.handle(&sys(16, true)), Action::Pass);
    }

    #[test]
    fn scroll_scaling() {
        assert_eq!(scale_delta(0, 3.0), 0);
        assert_eq!(scale_delta(4, 2.5), 10);
        assert_eq!(scale_delta(-4, 2.5), -10);
        assert_eq!(scale_delta(1, 0.25), 1);
        assert_eq!(scale_delta(-1, 0.25), -1);
        assert_eq!(scale_delta(10, 0.5), 5);
    }

    #[test]
    fn labels() {
        assert_eq!(
            describe(&LockTrigger::Key {
                keycode: 12,
                flags: CTRL_CMD
            }),
            "keycode 12 + ⌃⌘"
        );
        assert_eq!(
            describe(&LockTrigger::Key {
                keycode: 53,
                flags: 0
            }),
            "keycode 53"
        );
        assert_eq!(
            describe(&LockTrigger::System {
                subtype: 8,
                data1_keycode: 6
            }),
            "system key 6"
        );
    }
}
