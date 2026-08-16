//! The background worker pool.
//!
//! §3 constrains this tightly: a bounded pool of `min(num_cpus / 2, 4)`
//! threads at the lowest OS priority available, suspended when the window
//! loses focus, and no polling anywhere. Idle threads block on a condvar,
//! which costs exactly zero CPU, and completed work wakes the UI by calling
//! `Context::request_repaint` — never by the UI checking a flag every frame.

use anyhow::{bail, Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use roughcut_core::model::ClipId;
use roughcut_core::probe::{probe, MediaInfo};
use roughcut_core::proxy;
use roughcut_core::time::{frame_to_seconds, Rational};
use roughcut_core::tools::{quiet_command, Tools};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Bin thumbnail width in pixels. Small on purpose: 50 clips of these must fit
/// inside the 300 MB RSS budget alongside everything else.
pub const THUMB_WIDTH: u32 = 128;

#[derive(Debug, Clone)]
pub enum Job {
    Probe {
        path: PathBuf,
    },
    Thumbnail {
        clip_id: ClipId,
        path: PathBuf,
        frame: i64,
        fps: Rational,
    },
    Proxy {
        clip_id: ClipId,
        source: PathBuf,
        info: Box<MediaInfo>,
        proxy_dir: PathBuf,
    },
}

/// A decoded thumbnail, kept UI-framework-free so workers never touch egui.
pub struct ThumbData {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for ThumbData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbData")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

#[derive(Debug)]
pub enum JobResult {
    Probed {
        path: PathBuf,
        result: Result<MediaInfo>,
    },
    Thumbnail {
        clip_id: ClipId,
        result: Result<ThumbData>,
    },
    ProxyStarted {
        clip_id: ClipId,
    },
    ProxyDone {
        clip_id: ClipId,
        result: Result<PathBuf>,
    },
}

struct Queue {
    jobs: VecDeque<Job>,
    suspended: bool,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    tx: Sender<JobResult>,
    ctx: egui::Context,
    tools: Tools,
}

pub struct WorkerPool {
    shared: Arc<Shared>,
    results: Receiver<JobResult>,
    threads: Vec<JoinHandle<()>>,
}

/// §3: `min(num_cpus / 2, 4)`, and at least one.
pub fn pool_size() -> usize {
    (num_cpus::get() / 2).clamp(1, 4)
}

impl WorkerPool {
    pub fn new(ctx: egui::Context, tools: Tools) -> Self {
        let (tx, results) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                suspended: false,
                shutdown: false,
            }),
            wake: Condvar::new(),
            tx,
            ctx,
            tools,
        });

        let threads = (0..pool_size())
            .map(|i| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("roughcut-worker-{i}"))
                    .spawn(move || {
                        lower_thread_priority();
                        worker_loop(shared);
                    })
                    .expect("cannot spawn worker thread")
            })
            .collect();

        Self {
            shared,
            results,
            threads,
        }
    }

    pub fn submit(&self, job: Job) {
        {
            let mut q = self.shared.queue.lock().unwrap();
            if q.shutdown {
                return;
            }
            q.jobs.push_back(job);
        }
        self.shared.wake.notify_one();
    }

    /// Stop handing out new work. In-flight jobs run to completion — an
    /// ffmpeg child process cannot be paused mid-transcode without leaving
    /// a half-written file behind.
    pub fn set_suspended(&self, suspended: bool) {
        let changed = {
            let mut q = self.shared.queue.lock().unwrap();
            let changed = q.suspended != suspended;
            q.suspended = suspended;
            changed
        };
        if changed && !suspended {
            self.shared.wake.notify_all();
        }
    }

    /// Drain finished work. Never blocks.
    pub fn poll(&self) -> impl Iterator<Item = JobResult> + '_ {
        self.results.try_iter()
    }

    /// Drop everything not yet started, e.g. when a project is closed.
    pub fn clear_queue(&self) {
        let mut q = self.shared.queue.lock().unwrap();
        q.jobs.clear();
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        {
            let mut q = self.shared.queue.lock().unwrap();
            q.shutdown = true;
            q.jobs.clear();
        }
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        // Block until there is work and we are not suspended. A condvar wait
        // is a real OS sleep: no timer, no wakeups, no CPU.
        let job = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                if !q.suspended {
                    if let Some(job) = q.jobs.pop_front() {
                        break job;
                    }
                }
                q = shared.wake.wait(q).unwrap();
            }
        };

        let result = run_job(&shared, job);

        if shared.tx.send(result).is_err() {
            return;
        }
        // Wake the UI thread so it can pick the result up. This is the only
        // mechanism by which background work causes a repaint.
        shared.ctx.request_repaint();
    }
}

fn run_job(shared: &Shared, job: Job) -> JobResult {
    match job {
        Job::Probe { path } => {
            let result = match &shared.tools.ffprobe {
                Some(ffprobe) => probe(ffprobe, &path),
                None => Err(anyhow::anyhow!("ffprobe is not available")),
            };
            JobResult::Probed { path, result }
        }
        Job::Thumbnail {
            clip_id,
            path,
            frame,
            fps,
        } => {
            let result = match &shared.tools.ffmpeg {
                Some(ffmpeg) => make_thumbnail(ffmpeg, &path, frame, fps),
                None => Err(anyhow::anyhow!("ffmpeg is not available")),
            };
            JobResult::Thumbnail { clip_id, result }
        }
        Job::Proxy {
            clip_id,
            source,
            info,
            proxy_dir,
        } => {
            let _ = shared.tx.send(JobResult::ProxyStarted { clip_id });
            shared.ctx.request_repaint();
            let result = match (&shared.tools.ffmpeg, &shared.tools.ffprobe) {
                (Some(ffmpeg), Some(ffprobe)) => {
                    // `generate` only needs the id and path off the clip.
                    let stub = roughcut_core::model::SourceClip {
                        id: clip_id,
                        path: source,
                        proxy_path: None,
                        duration_frames: info.native_frames,
                        native_fps_num: info.fps.num,
                        native_fps_den: info.fps.den,
                        has_audio: info.has_audio,
                        video_index: info.video_index,
                        audio_index: info.audio_index,
                        mark_in: None,
                        mark_out: None,
                        rate_mismatch: false,
                    };
                    proxy::generate(ffmpeg, ffprobe, &stub, &info, &proxy_dir)
                }
                _ => Err(anyhow::anyhow!(
                    "proxy generation needs both ffmpeg and ffprobe"
                )),
            };
            JobResult::ProxyDone { clip_id, result }
        }
    }
}

/// One frame, scaled down, straight out of ffmpeg as a PNG on stdout.
fn make_thumbnail(
    ffmpeg: &std::path::Path,
    path: &std::path::Path,
    frame: i64,
    fps: Rational,
) -> Result<ThumbData> {
    // Input seek (`-ss` before `-i`) is orders of magnitude faster than output
    // seek and is accurate enough for a bin thumbnail.
    let seconds = frame_to_seconds(frame, fps);
    let output = quiet_command(ffmpeg)
        .args(["-v", "error", "-ss", &format!("{seconds:.6}")])
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={THUMB_WIDTH}:-2:flags=fast_bilinear"),
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-",
        ])
        .output()
        .with_context(|| format!("cannot run ffmpeg at {}", ffmpeg.display()))?;

    if !output.status.success() || output.stdout.is_empty() {
        bail!(
            "ffmpeg produced no thumbnail: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let img = image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Png)
        .context("cannot decode the thumbnail ffmpeg produced")?
        .to_rgba8();
    Ok(ThumbData {
        width: img.width() as usize,
        height: img.height() as usize,
        rgba: img.into_raw(),
    })
}

/// Drop the calling thread to the lowest priority the OS offers, so a thumbnail
/// sweep never makes the UI stutter.
fn lower_thread_priority() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
            THREAD_PRIORITY_LOWEST,
        };
        // SAFETY: both calls take the pseudo-handle for the current thread and
        // cannot fail in a way that matters; the result is advisory.
        unsafe {
            let h = GetCurrentThread();
            SetThreadPriority(h, THREAD_PRIORITY_LOWEST);
            // Background mode also lowers I/O priority, which matters more
            // than CPU priority for ffmpeg jobs.
            SetThreadPriority(h, THREAD_MODE_BACKGROUND_BEGIN);
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Quality-of-Service is the only thread-scoped priority control on
        // macOS. `setpriority(PRIO_PROCESS, 0, …)` would be wrong here: unlike
        // Linux, where it happens to affect just the calling thread, on macOS
        // it renices the *whole process* — including the UI thread, which is
        // the exact opposite of the intent.
        //
        // BACKGROUND throttles I/O as well as CPU, which matters more than CPU
        // priority for jobs that are really ffmpeg reading files.
        const QOS_CLASS_BACKGROUND: u32 = 0x09;
        extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        // SAFETY: operates on the calling thread and cannot fail in a way that
        // matters; the result is advisory.
        unsafe {
            pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0);
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // On Linux `setpriority(PRIO_PROCESS, 0, …)` applies to the calling
        // thread, which is what is wanted.
        extern "C" {
            fn setpriority(which: i32, who: u32, prio: i32) -> i32;
        }
        // SAFETY: PRIO_PROCESS with who = 0 is always well defined.
        unsafe {
            setpriority(0, 0, 10);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_size_is_bounded() {
        let n = pool_size();
        assert!((1..=4).contains(&n), "pool size {n} outside the §3 bound");
        assert!(n <= (num_cpus::get() / 2).max(1));
    }
}
