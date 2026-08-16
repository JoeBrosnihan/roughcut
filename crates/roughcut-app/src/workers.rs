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

/// Width of one filmstrip tile, in pixels. Small on purpose: 50 clips of these
/// must fit inside the 300 MB RSS budget alongside everything else.
pub const THUMB_WIDTH: u32 = 96;

/// Frames sampled across each clip for hover-scrubbing in the bin.
///
/// They are baked into one horizontal sheet per clip rather than fetched as
/// the pointer moves. Hovering then costs a UV offset and nothing else — no
/// decode, no I/O, no work while the pointer is still.
pub const FILMSTRIP_FRAMES: usize = 12;

#[derive(Debug, Clone)]
pub enum Job {
    Probe {
        path: PathBuf,
    },
    Filmstrip {
        clip_id: ClipId,
        path: PathBuf,
        duration_frames: i64,
        fps: Rational,
    },
    Proxy {
        clip_id: ClipId,
        source: PathBuf,
        info: Box<MediaInfo>,
        proxy_dir: PathBuf,
    },
}

/// `FILMSTRIP_FRAMES` tiles laid out left to right in one image. Kept
/// UI-framework-free so workers never touch egui.
pub struct Filmstrip {
    /// Width of the whole sheet, i.e. one tile times `FILMSTRIP_FRAMES`.
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Filmstrip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filmstrip")
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
    Filmstrip {
        clip_id: ClipId,
        result: Result<Filmstrip>,
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
        Job::Filmstrip {
            clip_id,
            path,
            duration_frames,
            fps,
        } => {
            let result = match &shared.tools.ffmpeg {
                Some(ffmpeg) => make_filmstrip(ffmpeg, &path, duration_frames, fps),
                None => Err(anyhow::anyhow!("ffmpeg is not available")),
            };
            JobResult::Filmstrip { clip_id, result }
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
/// The frame sampled for tile `i`: the centre of the slice it represents, so
/// tile 0 is not frame 0 — the first frame of a shot is very often black.
pub fn filmstrip_frame(tile: usize, duration_frames: i64) -> i64 {
    let last = (duration_frames - 1).max(0);
    let n = FILMSTRIP_FRAMES as i64;
    ((tile as i64 * 2 + 1) * last) / (n * 2)
}

/// Sample `FILMSTRIP_FRAMES` frames across the clip and lay them out in one
/// horizontal sheet.
///
/// Each frame is a separate input-seek grab, which is near-instant regardless
/// of how long the file is. A single ffmpeg call with an `fps` filter would be
/// tidier but has to decode the whole file — minutes, for a long 4K interview.
fn make_filmstrip(
    ffmpeg: &std::path::Path,
    path: &std::path::Path,
    duration_frames: i64,
    fps: Rational,
) -> Result<Filmstrip> {
    let tiles: Vec<Option<image::RgbaImage>> = (0..FILMSTRIP_FRAMES)
        .map(|i| grab_frame(ffmpeg, path, filmstrip_frame(i, duration_frames), fps).ok())
        .collect();

    let first = tiles
        .iter()
        .flatten()
        .next()
        .context("ffmpeg produced no frames for this clip")?;
    let (tw, th) = (first.width(), first.height());

    // Anything that failed or came back an odd size is left black rather than
    // shifting every later tile along.
    let mut sheet = image::RgbaImage::new(tw * FILMSTRIP_FRAMES as u32, th);
    for (i, tile) in tiles.iter().enumerate() {
        let Some(img) = tile else { continue };
        if img.width() != tw || img.height() != th {
            continue;
        }
        let x0 = i as u32 * tw;
        for y in 0..th {
            for x in 0..tw {
                sheet.put_pixel(x0 + x, y, *img.get_pixel(x, y));
            }
        }
    }

    Ok(Filmstrip {
        width: sheet.width() as usize,
        height: sheet.height() as usize,
        rgba: sheet.into_raw(),
    })
}

fn grab_frame(
    ffmpeg: &std::path::Path,
    path: &std::path::Path,
    frame: i64,
    fps: Rational,
) -> Result<image::RgbaImage> {
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

    Ok(
        image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Png)
            .context("cannot decode the thumbnail ffmpeg produced")?
            .to_rgba8(),
    )
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
    fn filmstrip_tiles_span_the_clip_in_order() {
        let last = 299;
        let frames: Vec<i64> = (0..FILMSTRIP_FRAMES)
            .map(|i| filmstrip_frame(i, 300))
            .collect();
        assert_eq!(frames.len(), FILMSTRIP_FRAMES);
        // Never the very first frame, which is usually black.
        assert!(frames[0] > 0, "tile 0 sampled frame {}", frames[0]);
        assert!(frames.windows(2).all(|w| w[0] < w[1]), "{frames:?}");
        assert!(*frames.last().unwrap() <= last);
    }

    #[test]
    fn filmstrip_handles_degenerate_clips() {
        // A one-frame clip: every tile is frame 0, and nothing panics.
        for i in 0..FILMSTRIP_FRAMES {
            assert_eq!(filmstrip_frame(i, 1), 0);
            assert_eq!(filmstrip_frame(i, 0), 0);
        }
    }

    #[test]
    fn pool_size_is_bounded() {
        let n = pool_size();
        assert!((1..=4).contains(&n), "pool size {n} outside the §3 bound");
        assert!(n <= (num_cpus::get() / 2).max(1));
    }
}
