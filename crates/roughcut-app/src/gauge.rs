//! Where the memory went.
//!
//! The model crate's child ledger (`roughcut_core::tools::run`) says *that* a
//! proxy or a filmstrip is running; this module watches *what it costs*. It
//! installs itself as the ledger's observer, and while any child is alive a
//! sampler thread reads each one's working set twice a second. From that it
//! keeps three things:
//!
//! 1. **A live line** — "3 children, 2.1 GB" — shown in the `?` overlay, so
//!    "why is the machine slow" can be answered by looking at it.
//! 2. **A record per finished child** — label, seconds, peak memory — logged,
//!    loudly when the peak was large, and folded into per-label statistics
//!    that are written out on exit next to the seek-latency summary.
//! 3. **A budget** — when the children together cross it (default 4 GB,
//!    `ROUGHCUT_CHILD_RAM_BUDGET_MB`), the worker pool stops starting new
//!    speculative work until enough of what is running has finished. The
//!    application manages its own appetite; nothing is surfaced for the user
//!    to act on, because there is nothing they should have to do. The hold
//!    is logged, and the `?` overlay's work line says so while it lasts.
//!
//! Idle cost is zero. The sampler blocks on a condvar whenever no child is
//! alive, which is §3's rule applied to diagnostics: watching for a problem
//! must not itself be one.

use std::sync::{Condvar, Mutex, OnceLock};

/// One child, alive right now.
struct Live {
    pid: u32,
    label: &'static str,
    peak_bytes: u64,
    last_bytes: u64,
}

/// Everything a label has ever done, for the exit summary.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelStat {
    pub label: String,
    pub runs: u64,
    pub failures: u64,
    pub total_seconds: f64,
    pub worst_seconds: f64,
    pub worst_bytes: u64,
}

#[derive(Default)]
struct State {
    live: Vec<Live>,
    stats: Vec<LabelStat>,
    /// Set when the budget is crossed, cleared when usage falls back under
    /// half of it. While set, the worker pool starts no new speculative work
    /// — that hysteresis is what stops the pool flapping at the boundary,
    /// starting one job per sample only to trip the budget again.
    over_budget: bool,
    most_at_once: usize,
    most_bytes: u64,
}

struct Gauge {
    state: Mutex<State>,
    wake: Condvar,
    budget_bytes: u64,
}

static GAUGE: OnceLock<Gauge> = OnceLock::new();

/// Told when the budget stops being exceeded, so the worker pool's condvar
/// can be woken — the workers held by the budget are asleep on it with
/// nothing else coming to wake them.
#[allow(clippy::type_complexity)]
static RELIEF: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);

const DEFAULT_BUDGET_MB: u64 = 4 * 1024;

fn budget_from_env() -> u64 {
    std::env::var("ROUGHCUT_CHILD_RAM_BUDGET_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|mb| *mb > 0)
        .unwrap_or(DEFAULT_BUDGET_MB)
        * 1024
        * 1024
}

/// Wire the gauge into the ledger and start the sampler. Call once, at
/// startup; later calls do nothing.
pub fn install() {
    let _ = GAUGE.get_or_init(|| Gauge {
        state: Mutex::new(State::default()),
        wake: Condvar::new(),
        budget_bytes: budget_from_env(),
    });

    roughcut_core::tools::observe_runs(Box::new(|event| {
        let Some(gauge) = GAUGE.get() else { return };
        match event {
            roughcut_core::tools::RunEvent::Started { pid, label } => {
                gauge.started(pid, leak(label))
            }
            roughcut_core::tools::RunEvent::Finished { pid, label, seconds, ok } => {
                gauge.finished(pid, label, seconds, ok)
            }
        }
    }));

    std::thread::Builder::new()
        .name("roughcut-gauge".into())
        .spawn(|| GAUGE.get().expect("installed above").sample_forever())
        .expect("cannot spawn the gauge thread");
}

/// The ledger hands out borrowed labels; the live table needs them to outlive
/// the call. Labels are a handful of fixed words ("proxy", "sheet", ...), so
/// interning them by leaking is a few bytes once, not a leak that grows.
fn leak(label: &str) -> &'static str {
    static KNOWN: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut known = KNOWN.lock().unwrap();
    if let Some(k) = known.iter().find(|k| **k == label) {
        return k;
    }
    let stat: &'static str = Box::leak(label.to_string().into_boxed_str());
    known.push(stat);
    stat
}

impl Gauge {
    fn started(&self, pid: u32, label: &'static str) {
        let mut st = self.state.lock().unwrap();
        st.live.push(Live { pid, label, peak_bytes: 0, last_bytes: 0 });
        st.most_at_once = st.most_at_once.max(st.live.len());
        drop(st);
        self.wake.notify_one();
    }

    fn finished(&self, pid: u32, label: &str, seconds: f64, ok: bool) {
        let mut st = self.state.lock().unwrap();
        let peak = match st.live.iter().position(|l| l.pid == pid) {
            Some(at) => st.live.swap_remove(at).peak_bytes,
            None => 0,
        };
        let stat = match st.stats.iter_mut().find(|s| s.label == label) {
            Some(s) => s,
            None => {
                st.stats.push(LabelStat {
                    label: label.to_string(),
                    runs: 0,
                    failures: 0,
                    total_seconds: 0.0,
                    worst_seconds: 0.0,
                    worst_bytes: 0,
                });
                st.stats.last_mut().expect("just pushed")
            }
        };
        stat.runs += 1;
        stat.failures += u64::from(!ok);
        stat.total_seconds += seconds;
        stat.worst_seconds = stat.worst_seconds.max(seconds);
        stat.worst_bytes = stat.worst_bytes.max(peak);
        // A child leaving can be what lifts the hold, and once the last one is
        // gone the sampler is asleep — this is the only place left to notice.
        let total: u64 = st.live.iter().map(|l| l.last_bytes).sum();
        let relieved = st.over_budget && (st.live.is_empty() || total < self.budget_bytes / 2);
        if relieved {
            st.over_budget = false;
        }
        drop(st);
        if relieved {
            relieve();
        }

        // A child that was big or slow earns a line; the rest stay at debug
        // so a working session does not scroll the log off the screen.
        let mb = peak / (1024 * 1024);
        if mb >= 512 || seconds >= 30.0 {
            log::info!("{label}: {seconds:.1}s, peak {mb} MB");
        } else {
            log::debug!("{label}: {seconds:.1}s, peak {mb} MB");
        }
    }

    /// The sampler. Asleep — properly asleep, on a condvar — whenever the
    /// live table is empty.
    fn sample_forever(&self) {
        let mut st = self.state.lock().unwrap();
        loop {
            while st.live.is_empty() {
                st = self.wake.wait(st).unwrap();
            }
            for child in st.live.iter_mut() {
                if let Some(bytes) = working_set(child.pid) {
                    child.last_bytes = bytes;
                    child.peak_bytes = child.peak_bytes.max(bytes);
                }
            }
            let total: u64 = st.live.iter().map(|l| l.last_bytes).sum();
            st.most_bytes = st.most_bytes.max(total);

            if total > self.budget_bytes && !st.over_budget {
                st.over_budget = true;
                let worst = st
                    .live
                    .iter()
                    .max_by_key(|l| l.last_bytes)
                    .map(|l| format!("{} at {}", l.label, human(l.last_bytes)))
                    .unwrap_or_default();
                // For the log, not the user: the hold is the application
                // managing itself, and there is nothing anyone should do
                // about it.
                log::info!(
                    "background work is holding {} ({} children; worst: {}) — pausing new work until it settles",
                    human(total),
                    st.live.len(),
                    worst
                );
            } else if st.over_budget && total < self.budget_bytes / 2 {
                st.over_budget = false;
                drop(st);
                relieve();
                st = self.state.lock().unwrap();
            }

            drop(st);
            std::thread::sleep(std::time::Duration::from_millis(500));
            st = self.state.lock().unwrap();
        }
    }
}

/// Whether the children together hold more memory than the budget allows.
/// The worker pool asks before starting anything speculative; work the user
/// is waiting on is never held. Always room when no gauge is installed — the
/// tests and the CLI have no sampler, so a stale `true` could never clear.
pub fn over_budget() -> bool {
    GAUGE
        .get()
        .is_some_and(|g| g.state.lock().unwrap().over_budget)
}

/// Register the wake-up call for when the budget stops being exceeded. The
/// last registration wins; there is one pool.
pub fn on_relief(f: impl Fn() + Send + Sync + 'static) {
    *RELIEF.lock().unwrap() = Some(Box::new(f));
}

fn relieve() {
    if let Some(f) = RELIEF.lock().unwrap().as_ref() {
        f();
    }
}

/// One line for the help overlay, or `None` when nothing is running.
pub fn live_line() -> Option<String> {
    let gauge = GAUGE.get()?;
    let st = gauge.state.lock().unwrap();
    if st.live.is_empty() {
        return None;
    }
    let total: u64 = st.live.iter().map(|l| l.last_bytes).sum();
    let mut labels: Vec<&str> = st.live.iter().map(|l| l.label).collect();
    labels.sort_unstable();
    labels.dedup();
    // The hold is the pool managing its own appetite, but while it is on,
    // "why has the queue stopped" deserves an answer in the same place as
    // "where did the memory go".
    let held = if st.over_budget {
        " — over budget, new work is waiting"
    } else {
        ""
    };
    Some(format!(
        "{} running ({}), {}{held}",
        st.live.len(),
        labels.join(", "),
        human(total)
    ))
}

/// Written on exit, beside the seek-latency summary. Says nothing when
/// nothing ran.
pub fn log_summary() {
    let Some(gauge) = GAUGE.get() else { return };
    let st = gauge.state.lock().unwrap();
    if st.stats.is_empty() {
        return;
    }
    log::info!(
        "child processes this session (most at once: {}, peak together: {}):",
        st.most_at_once,
        human(st.most_bytes)
    );
    for line in summary_lines(&st.stats) {
        log::info!("  {line}");
    }
}

/// The summary, as text — split from the logging so a test can hold it still.
pub fn summary_lines(stats: &[LabelStat]) -> Vec<String> {
    let mut ordered: Vec<&LabelStat> = stats.iter().collect();
    ordered.sort_by(|a, b| b.worst_bytes.cmp(&a.worst_bytes));
    ordered
        .iter()
        .map(|s| {
            let failures = if s.failures > 0 {
                format!(", {} failed", s.failures)
            } else {
                String::new()
            };
            format!(
                "{:8} x{:<4} total {:6.1}s, worst {:5.1}s, peak {}{}",
                s.label,
                s.runs,
                s.total_seconds,
                s.worst_seconds,
                human(s.worst_bytes),
                failures
            )
        })
        .collect()
}

fn human(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!("{} MB", bytes / (1024 * 1024))
    }
}

/// A process's current working set, or `None` once it has gone.
#[cfg(windows)]
fn working_set(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: a query-only handle on a pid we spawned; closed on every path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = GetProcessMemoryInfo(handle, &mut counters, counters.cb);
        CloseHandle(handle);
        (ok != 0).then_some(counters.WorkingSetSize as u64)
    }
}

#[cfg(not(windows))]
fn working_set(_pid: u32) -> Option<u64> {
    // The ledger still counts and times everything; only the bytes are
    // missing. Fill this in from /proc/<pid>/statm when a Unix build is real.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_leads_with_the_biggest_spender() {
        let stats = vec![
            LabelStat {
                label: "poster".into(),
                runs: 40,
                failures: 0,
                total_seconds: 40.0,
                worst_seconds: 2.0,
                worst_bytes: 200 * 1024 * 1024,
            },
            LabelStat {
                label: "sheet".into(),
                runs: 30,
                failures: 2,
                total_seconds: 300.0,
                worst_seconds: 20.0,
                worst_bytes: 3 * 1024 * 1024 * 1024,
            },
        ];
        let lines = summary_lines(&stats);
        assert!(lines[0].starts_with("sheet"), "{lines:?}");
        assert!(lines[0].contains("3.0 GB"), "{lines:?}");
        assert!(lines[0].contains("2 failed"), "{lines:?}");
        assert!(lines[1].contains("200 MB"), "{lines:?}");
        assert!(!lines[1].contains("failed"), "a clean label says nothing about failure");
    }

    /// The whole path at once: install, run a real child through the ledger,
    /// and find its cost in the statistics. This is the wiring the unit tests
    /// above cannot see — observer to registry to sampler to summary.
    #[test]
    fn a_real_child_lands_in_the_statistics_with_its_memory() {
        let Some(ffmpeg) = roughcut_core::tools::find_tool("ffmpeg") else {
            eprintln!("skipped: no ffmpeg on this machine");
            return;
        };
        install();

        // Two seconds of synthetic video, held to wall-clock time with `-re`
        // — without it x264 finishes the whole thing in a fraction of a
        // second and the 500ms sampler never catches it alive.
        let mut cmd = roughcut_core::tools::background_command(&ffmpeg);
        cmd.args([
            "-v", "error", "-re", "-f", "lavfi", "-i", "testsrc=size=1280x720:rate=30",
            "-t", "2", "-c:v", "libx264", "-f", "null", "-",
        ]);
        let out = roughcut_core::tools::run("gauge-test", &mut cmd).expect("ffmpeg runs");
        assert!(out.status.success());

        let gauge = GAUGE.get().expect("installed above");
        let st = gauge.state.lock().unwrap();
        let stat = st
            .stats
            .iter()
            .find(|s| s.label == "gauge-test")
            .expect("the run was recorded");
        assert_eq!(stat.runs, 1);
        assert_eq!(stat.failures, 0);
        assert!(stat.worst_seconds > 0.5, "an encode takes real time");
        #[cfg(windows)]
        assert!(
            stat.worst_bytes > 10 * 1024 * 1024,
            "the sampler saw its memory: {} bytes",
            stat.worst_bytes
        );
        // This child, specifically — not the whole table. The gauge is
        // process-wide and the suite runs in parallel, so another test's
        // ffmpeg can legitimately be alive in here at this moment; asserting
        // the table was empty made a passing suite depend on scheduling.
        assert!(
            !st.live.iter().any(|l| l.label == "gauge-test"),
            "the child was left on the live table after it finished"
        );
    }

    /// The FFI path, proven against the one process guaranteed to exist.
    #[cfg(windows)]
    #[test]
    fn a_working_set_can_actually_be_read() {
        let bytes = working_set(std::process::id()).expect("our own process is measurable");
        assert!(bytes > 1024 * 1024, "a running test holds more than a megabyte");
        assert!(working_set(4_000_000_000).is_none(), "a pid that is not there is None");
    }

    #[test]
    fn sizes_read_the_way_a_person_says_them() {
        assert_eq!(human(200 * 1024 * 1024), "200 MB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
