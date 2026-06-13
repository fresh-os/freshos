#![allow(dead_code)]

use core::sync::atomic::{AtomicU64, Ordering};

const MAX_TASKS: usize = 16;
const MAX_SURFACES: usize = 4;

#[derive(Clone, Copy)]
pub struct MetricSample {
    pub latest: u64,
    pub max: u64,
}

#[derive(Clone, Copy)]
pub struct DamageRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy)]
pub struct Snapshot {
    pub frame_us: MetricSample,
    pub present_us: MetricSample,
    pub background_us: MetricSample,
    pub windows_us: MetricSample,
    pub chrome_us: MetricSample,
    pub input_to_photon_us: MetricSample,
    pub ipc_rtt_us: MetricSample,
    pub sched_wake_us: MetricSample,
}

static FRAME_US_LATEST: AtomicU64 = AtomicU64::new(0);
static FRAME_US_MAX: AtomicU64 = AtomicU64::new(0);

static PRESENT_US_LATEST: AtomicU64 = AtomicU64::new(0);
static PRESENT_US_MAX: AtomicU64 = AtomicU64::new(0);

static BACKGROUND_US_LATEST: AtomicU64 = AtomicU64::new(0);
static BACKGROUND_US_MAX: AtomicU64 = AtomicU64::new(0);

static WINDOWS_US_LATEST: AtomicU64 = AtomicU64::new(0);
static WINDOWS_US_MAX: AtomicU64 = AtomicU64::new(0);

static CHROME_US_LATEST: AtomicU64 = AtomicU64::new(0);
static CHROME_US_MAX: AtomicU64 = AtomicU64::new(0);

static INPUT_TO_PHOTON_US_LATEST: AtomicU64 = AtomicU64::new(0);
static INPUT_TO_PHOTON_US_MAX: AtomicU64 = AtomicU64::new(0);

static IPC_RTT_US_LATEST: AtomicU64 = AtomicU64::new(0);
static IPC_RTT_US_MAX: AtomicU64 = AtomicU64::new(0);

static SCHED_WAKE_US_LATEST: AtomicU64 = AtomicU64::new(0);
static SCHED_WAKE_US_MAX: AtomicU64 = AtomicU64::new(0);

static WAKE_MARK_NS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

static SURFACE_SEQ: [AtomicU64; MAX_SURFACES] = [const { AtomicU64::new(0) }; MAX_SURFACES];
static SURFACE_DAMAGE_X: [AtomicU64; MAX_SURFACES] = [const { AtomicU64::new(0) }; MAX_SURFACES];
static SURFACE_DAMAGE_Y: [AtomicU64; MAX_SURFACES] = [const { AtomicU64::new(0) }; MAX_SURFACES];
static SURFACE_DAMAGE_W: [AtomicU64; MAX_SURFACES] = [const { AtomicU64::new(0) }; MAX_SURFACES];
static SURFACE_DAMAGE_H: [AtomicU64; MAX_SURFACES] = [const { AtomicU64::new(0) }; MAX_SURFACES];
static SHELL_INPUT_NS: AtomicU64 = AtomicU64::new(0);

#[inline]
fn store_sample(latest: &AtomicU64, max: &AtomicU64, value: u64) {
    latest.store(value, Ordering::Relaxed);
    max.fetch_max(value, Ordering::Relaxed);
}

pub fn record_frame_us(value: u64) {
    store_sample(&FRAME_US_LATEST, &FRAME_US_MAX, value);
}

pub fn record_present_us(value: u64) {
    store_sample(&PRESENT_US_LATEST, &PRESENT_US_MAX, value);
}

pub fn record_background_us(value: u64) {
    store_sample(&BACKGROUND_US_LATEST, &BACKGROUND_US_MAX, value);
}

pub fn record_windows_us(value: u64) {
    store_sample(&WINDOWS_US_LATEST, &WINDOWS_US_MAX, value);
}

pub fn record_chrome_us(value: u64) {
    store_sample(&CHROME_US_LATEST, &CHROME_US_MAX, value);
}

pub fn record_input_to_photon_us(value: u64) {
    store_sample(&INPUT_TO_PHOTON_US_LATEST, &INPUT_TO_PHOTON_US_MAX, value);
}

pub fn record_ipc_rtt_us(value: u64) {
    store_sample(&IPC_RTT_US_LATEST, &IPC_RTT_US_MAX, value);
}

pub fn record_sched_wake_us(value: u64) {
    store_sample(&SCHED_WAKE_US_LATEST, &SCHED_WAKE_US_MAX, value);
}

pub fn note_task_unblocked(task_id: usize, at_ns: u64) {
    if task_id < MAX_TASKS {
        WAKE_MARK_NS[task_id].store(at_ns, Ordering::Relaxed);
    }
}

pub fn note_task_running(task_id: usize, now_ns: u64) {
    if task_id >= MAX_TASKS {
        return;
    }

    let mark = WAKE_MARK_NS[task_id].swap(0, Ordering::Relaxed);
    if mark > 0 && now_ns > mark {
        record_sched_wake_us((now_ns - mark) / 1000);
    }
}

fn merge_surface_damage(surface_id: usize, x: u32, y: u32, w: u32, h: u32) {
    if surface_id >= MAX_SURFACES || w == 0 || h == 0 {
        return;
    }

    let cur_w = SURFACE_DAMAGE_W[surface_id].load(Ordering::Relaxed) as u32;
    let cur_h = SURFACE_DAMAGE_H[surface_id].load(Ordering::Relaxed) as u32;
    if cur_w == 0 || cur_h == 0 {
        SURFACE_DAMAGE_X[surface_id].store(x as u64, Ordering::Relaxed);
        SURFACE_DAMAGE_Y[surface_id].store(y as u64, Ordering::Relaxed);
        SURFACE_DAMAGE_W[surface_id].store(w as u64, Ordering::Relaxed);
        SURFACE_DAMAGE_H[surface_id].store(h as u64, Ordering::Relaxed);
        return;
    }

    let cur_x = SURFACE_DAMAGE_X[surface_id].load(Ordering::Relaxed) as u32;
    let cur_y = SURFACE_DAMAGE_Y[surface_id].load(Ordering::Relaxed) as u32;
    let cur_r = cur_x.saturating_add(cur_w);
    let cur_b = cur_y.saturating_add(cur_h);
    let new_r = x.saturating_add(w);
    let new_b = y.saturating_add(h);

    let merged_x = cur_x.min(x);
    let merged_y = cur_y.min(y);
    let merged_r = cur_r.max(new_r);
    let merged_b = cur_b.max(new_b);

    SURFACE_DAMAGE_X[surface_id].store(merged_x as u64, Ordering::Relaxed);
    SURFACE_DAMAGE_Y[surface_id].store(merged_y as u64, Ordering::Relaxed);
    SURFACE_DAMAGE_W[surface_id].store(merged_r.saturating_sub(merged_x) as u64, Ordering::Relaxed);
    SURFACE_DAMAGE_H[surface_id].store(merged_b.saturating_sub(merged_y) as u64, Ordering::Relaxed);
}

pub fn mark_surface_commit(surface_id: usize) {
    if surface_id < MAX_SURFACES {
        SURFACE_SEQ[surface_id].fetch_add(1, Ordering::Relaxed);
    }
}

pub fn mark_surface_damage(surface_id: usize, x: u32, y: u32, w: u32, h: u32) {
    merge_surface_damage(surface_id, x, y, w, h);
    mark_surface_commit(surface_id);
}

pub fn surface_commit_seq(surface_id: usize) -> u64 {
    if surface_id < MAX_SURFACES {
        SURFACE_SEQ[surface_id].load(Ordering::Relaxed)
    } else {
        0
    }
}

pub fn surface_damage_snapshot(surface_id: usize) -> (u64, DamageRect) {
    if surface_id >= MAX_SURFACES {
        return (
            0,
            DamageRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
        );
    }

    loop {
        let seq_before = SURFACE_SEQ[surface_id].load(Ordering::Relaxed);
        let rect = DamageRect {
            x: SURFACE_DAMAGE_X[surface_id].load(Ordering::Relaxed) as u32,
            y: SURFACE_DAMAGE_Y[surface_id].load(Ordering::Relaxed) as u32,
            w: SURFACE_DAMAGE_W[surface_id].load(Ordering::Relaxed) as u32,
            h: SURFACE_DAMAGE_H[surface_id].load(Ordering::Relaxed) as u32,
        };
        let seq_after = SURFACE_SEQ[surface_id].load(Ordering::Relaxed);
        if seq_before == seq_after {
            return (seq_after, rect);
        }
    }
}

pub fn clear_surface_damage(surface_id: usize, seq: u64) {
    if surface_id >= MAX_SURFACES || SURFACE_SEQ[surface_id].load(Ordering::Relaxed) != seq {
        return;
    }

    SURFACE_DAMAGE_X[surface_id].store(0, Ordering::Relaxed);
    SURFACE_DAMAGE_Y[surface_id].store(0, Ordering::Relaxed);
    SURFACE_DAMAGE_W[surface_id].store(0, Ordering::Relaxed);
    SURFACE_DAMAGE_H[surface_id].store(0, Ordering::Relaxed);
}

pub fn mark_shell_surface_damage(input_ns: u64, x: u32, y: u32, w: u32, h: u32) {
    if input_ns == 0 {
        return;
    }
    SHELL_INPUT_NS.store(input_ns, Ordering::Relaxed);
    mark_surface_damage(0, x, y, w, h);
}

pub fn shell_surface_snapshot() -> (u64, u64) {
    let seq_before = surface_commit_seq(0);
    let input_ns = SHELL_INPUT_NS.load(Ordering::Relaxed);
    let seq_after = surface_commit_seq(0);
    if seq_before == seq_after {
        (seq_before, input_ns)
    } else {
        (seq_after, SHELL_INPUT_NS.load(Ordering::Relaxed))
    }
}

pub fn shell_input_ns() -> u64 {
    SHELL_INPUT_NS.load(Ordering::Relaxed)
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        frame_us: MetricSample {
            latest: FRAME_US_LATEST.load(Ordering::Relaxed),
            max: FRAME_US_MAX.load(Ordering::Relaxed),
        },
        present_us: MetricSample {
            latest: PRESENT_US_LATEST.load(Ordering::Relaxed),
            max: PRESENT_US_MAX.load(Ordering::Relaxed),
        },
        background_us: MetricSample {
            latest: BACKGROUND_US_LATEST.load(Ordering::Relaxed),
            max: BACKGROUND_US_MAX.load(Ordering::Relaxed),
        },
        windows_us: MetricSample {
            latest: WINDOWS_US_LATEST.load(Ordering::Relaxed),
            max: WINDOWS_US_MAX.load(Ordering::Relaxed),
        },
        chrome_us: MetricSample {
            latest: CHROME_US_LATEST.load(Ordering::Relaxed),
            max: CHROME_US_MAX.load(Ordering::Relaxed),
        },
        input_to_photon_us: MetricSample {
            latest: INPUT_TO_PHOTON_US_LATEST.load(Ordering::Relaxed),
            max: INPUT_TO_PHOTON_US_MAX.load(Ordering::Relaxed),
        },
        ipc_rtt_us: MetricSample {
            latest: IPC_RTT_US_LATEST.load(Ordering::Relaxed),
            max: IPC_RTT_US_MAX.load(Ordering::Relaxed),
        },
        sched_wake_us: MetricSample {
            latest: SCHED_WAKE_US_LATEST.load(Ordering::Relaxed),
            max: SCHED_WAKE_US_MAX.load(Ordering::Relaxed),
        },
    }
}
