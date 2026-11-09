import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface FeatureStatus {
  on: boolean;
  ok: boolean;
  message: string;
}
interface LockStatus extends FeatureStatus {
  trigger_label: string | null;
  accessibility: boolean;
  learning: boolean;
}
interface Status {
  colemak: FeatureStatus;
  caps_backspace: FeatureStatus;
  lock_sleep: LockStatus;
  scroll: { speed: number; ok: boolean; message: string };
}

const LEARN_TIMEOUT_MS = 15000;
const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const colemak = $<HTMLInputElement>("colemak");
const caps = $<HTMLInputElement>("caps");
const lock = $<HTMLInputElement>("lock");
const learn = $<HTMLButtonElement>("learn");
const ax = $<HTMLButtonElement>("ax");
const scroll = $<HTMLInputElement>("scroll");
const scrollVal = $("scroll-val");
// Slider position → speed. Finer steps below 1×.
const SPEEDS = [0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45, 0.5, 0.6, 0.7, 0.8, 0.9, 1, 1.25, 1.5, 2, 2.5, 3, 4, 5];
scroll.max = String(SPEEDS.length - 1);
const speed = () => SPEEDS[Number(scroll.value)];
const showSpeed = () => (scrollVal.textContent = `${speed()}×`);
const nearest = (v: number) =>
  SPEEDS.reduce((best, s, i) => (Math.abs(s - v) < Math.abs(SPEEDS[best] - v) ? i : best), 0);
let learnTimer: number | undefined;

function setMsg(id: string, text: string, ok: boolean) {
  const el = $(id);
  el.textContent = text;
  el.classList.toggle("bad", !ok);
}

function render(s: Status) {
  colemak.checked = s.colemak.on;
  setMsg("colemak-msg", s.colemak.message, s.colemak.ok);
  caps.checked = s.caps_backspace.on;
  setMsg("caps-msg", s.caps_backspace.message, s.caps_backspace.ok);

  const l = s.lock_sleep;
  lock.checked = l.on;
  lock.disabled = !l.trigger_label;
  setMsg("lock-msg", l.message, l.ok);
  $("lock-trigger").textContent = l.trigger_label ? `Key: ${l.trigger_label}` : "";
  learn.textContent = l.learning ? "Press the lock key…" : "Learn key";
  learn.disabled = l.learning;
  ax.hidden = l.accessibility;

  scroll.value = String(nearest(s.scroll.speed));
  showSpeed();
  setMsg("scroll-msg", s.scroll.message, s.scroll.ok);
}

async function call(cmd: string, args: Record<string, unknown>, errId: string) {
  try {
    render(await invoke<Status>(cmd, args));
    return true;
  } catch (e) {
    await refresh();
    setMsg(errId, String(e), false);
    return false;
  }
}

async function refresh() {
  try {
    render(await invoke<Status>("get_status"));
  } catch (e) {
    setMsg("lock-msg", String(e), false);
  }
}

colemak.addEventListener("change", () => call("set_colemak", { on: colemak.checked }, "colemak-msg"));
caps.addEventListener("change", () => call("set_caps_backspace", { on: caps.checked }, "caps-msg"));
lock.addEventListener("change", () => call("set_lock_sleep", { on: lock.checked }, "lock-msg"));
ax.addEventListener("click", () => invoke("open_accessibility_settings"));
scroll.addEventListener("input", showSpeed);
scroll.addEventListener("change", () => call("set_scroll_speed", { speed: speed() }, "scroll-msg"));
$("scroll-reset").addEventListener("click", () => call("set_scroll_speed", { speed: 1 }, "scroll-msg"));

// Arrow keys / Home / End move the scroll slider from anywhere in the window.
const STEP_KEYS: Record<string, number> = { ArrowLeft: -1, ArrowDown: -1, ArrowRight: 1, ArrowUp: 1 };
let scrollSave: number | undefined;
document.addEventListener("keydown", (e) => {
  const last = SPEEDS.length - 1;
  const i = Number(scroll.value);
  const next =
    e.key in STEP_KEYS ? i + STEP_KEYS[e.key] : e.key === "Home" ? 0 : e.key === "End" ? last : null;
  if (next === null || e.metaKey || e.ctrlKey || e.altKey) return;
  e.preventDefault();
  scroll.value = String(Math.max(0, Math.min(last, next)));
  showSpeed();
  // Save once the key is released / repeats stop.
  clearTimeout(scrollSave);
  scrollSave = window.setTimeout(() => call("set_scroll_speed", { speed: speed() }, "scroll-msg"), 250);
});

learn.addEventListener("click", async () => {
  if (!(await call("start_learn", {}, "lock-msg"))) return;
  clearTimeout(learnTimer);
  learnTimer = window.setTimeout(async () => {
    await call("cancel_learn", {}, "lock-msg");
    setMsg("lock-msg", "Lock key not detectable by this app", false);
  }, LEARN_TIMEOUT_MS);
});

listen<string>("lock-learned", async () => {
  clearTimeout(learnTimer);
  await refresh();
});

window.addEventListener("focus", refresh);
refresh();
