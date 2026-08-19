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
use roughcut_core::rotate::{self, Turn};
use roughcut_core::time::{frame_to_seconds, Rational};
use roughcut_core::tools::{quiet_command, Tools};
use roughcut_core::waveform;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Width of one filmstrip tile, in pixels. Small on purpose: 50 clips of these
/// must fit inside the 300 MB RSS budget alongside everything else.
pub const THUMB_WIDTH: u32 = 96;

/// Frames sampled across each clip for hover-scrubbing in the bin.
///
/// One per pixel of the tile's width, so moving the pointer a single pixel
/// always lands on a frame that was really extracted. Fewer and the picture
/// visibly steps as you skim; more would sample finer than the screen can
/// show.
///
/// They are baked into one sheet per clip rather than fetched as the pointer
/// moves. Hovering then costs a UV offset and nothing else — no decode, no
/// I/O, no work at all while the pointer is still.
pub const SCRUB_TILES: usize = 112;

/// Tiles per row in a sheet. A single row of 112 would be 10752 pixels wide,
/// near enough the maximum texture size on older hardware to be worth
/// avoiding; a grid keeps it to 1344x432.
const SHEET_COLS: usize = 14;

/// Columns and rows a sheet of `tiles` is laid out as.
pub fn grid_for(tiles: usize) -> (usize, usize) {
    if tiles <= 1 {
        return (1, 1);
    }
    let cols = SHEET_COLS.min(tiles);
    (cols, tiles.div_ceil(cols))
}

/// How many tiles are asked of ffmpeg at once.
///
/// Every tile is a separate `-i`, so a whole sheet in one command would build
/// a command line long enough to be a problem on Windows. Batching also means
/// one impossible seek costs a batch rather than the entire sheet.
const BATCH: usize = 16;

/// Work the user is waiting on goes first.
///
/// The bin is unusable until its pictures appear, so every clip gets its one
/// poster frame before any clip gets the 112-frame sheet that only matters
/// once you hover it. Without this a large bin spends minutes building scrub
/// data for the first clip while the fortieth is still a grey rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Runs on a reserved thread, so it starts even when every general worker
    /// is midway through a transcode.
    High,
    Low,
}

#[derive(Debug, Clone)]
pub enum Job {
    Probe {
        path: PathBuf,
    },
    /// Tiles for one clip: `tiles == 1` is the poster the bin shows at rest,
    /// anything more is the sheet hover-scrubbing indexes into.
    Thumbs {
        clip_id: ClipId,
        path: PathBuf,
        duration_frames: i64,
        fps: Rational,
        tiles: usize,
        /// A photograph: one frame, at the start, and no seeking to reach it.
        still: bool,
        /// Where finished sheets are kept between sessions. `None` disables
        /// caching, which only happens if there is nowhere to write.
        cache_dir: Option<PathBuf>,
    },
    Proxy {
        clip_id: ClipId,
        source: PathBuf,
        info: Box<MediaInfo>,
        proxy_dir: PathBuf,
    },
    /// Rewrite a source file's orientation in place. A stream copy, but on a
    /// large file still slow enough that the UI must not wait on it.
    Rotate {
        clip_id: ClipId,
        path: PathBuf,
        turn: Turn,
    },
    /// The loudness envelope drawn under the scrub bar.
    Waveform {
        clip_id: ClipId,
        path: PathBuf,
        cache_dir: Option<PathBuf>,
    },
}

/// Tiles packed into one image, row-major. Kept UI-framework-free so workers
/// never touch egui.
pub struct Sheet {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub cols: usize,
    pub rows: usize,
    /// Tiles actually filled. The last row may be short; spare cells stay
    /// black and are never indexed.
    pub tiles: usize,
}

impl std::fmt::Debug for Sheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sheet")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("tiles", &self.tiles)
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
    Thumbs {
        clip_id: ClipId,
        result: Result<Sheet>,
    },
    ProxyStarted {
        clip_id: ClipId,
    },
    ProxyDone {
        clip_id: ClipId,
        result: Result<PathBuf>,
    },
    Rotated {
        clip_id: ClipId,
        result: Result<MediaInfo>,
    },
    Waved {
        clip_id: ClipId,
        result: Result<Vec<u8>>,
    },
}

struct Queue {
    high: VecDeque<Job>,
    low: VecDeque<Job>,
    suspended: bool,
    shutdown: bool,
}

impl Job {
    fn priority(&self) -> Priority {
        match self {
            // Probing gates import, and a rotation is something the user just
            // asked for and is sitting there waiting on.
            // A waveform is only ever asked for about the clip open in the
            // monitor right now, which makes it the definition of work
            // somebody is waiting on.
            Job::Probe { .. } | Job::Rotate { .. } | Job::Waveform { .. } => Priority::High,
            Job::Thumbs { tiles, .. } if *tiles <= 1 => Priority::High,
            Job::Thumbs { .. } | Job::Proxy { .. } => Priority::Low,
        }
    }
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
                high: VecDeque::new(),
                low: VecDeque::new(),
                suspended: false,
                shutdown: false,
            }),
            wake: Condvar::new(),
            tx,
            ctx,
            tools,
        });

        // One thread beyond the pool takes nothing but high-priority work.
        //
        // Priority alone is not enough: it decides what comes off the queue
        // next, not who is free to take it. With every general worker part-way
        // through a multi-second transcode, a rotation the user just asked for
        // would sit there until one of them finished. This thread is asleep on
        // a condvar essentially always, so reserving it costs nothing.
        let threads = (0..pool_size() + 1)
            .map(|i| {
                let shared = shared.clone();
                let interactive = i == pool_size();
                let name = if interactive {
                    "roughcut-interactive".to_string()
                } else {
                    format!("roughcut-worker-{i}")
                };
                std::thread::Builder::new()
                    .name(name)
                    .spawn(move || {
                        // The reserved thread stays at normal priority: it
                        // exists to be responsive, and the OS should not
                        // deprioritise the work someone is waiting on.
                        if !interactive {
                            lower_thread_priority();
                        }
                        worker_loop(shared, interactive);
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
            match job.priority() {
                Priority::High => q.high.push_back(job),
                Priority::Low => q.low.push_back(job),
            }
        }
        // `notify_all`, not `notify_one`: the reserved thread ignores
        // low-priority work, so waking exactly one sleeper risks waking the
        // one that cannot take the job.
        self.shared.wake.notify_all();
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
        q.high.clear();
        q.low.clear();
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        {
            let mut q = self.shared.queue.lock().unwrap();
            q.shutdown = true;
            q.high.clear();
            q.low.clear();
        }
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn worker_loop(shared: Arc<Shared>, interactive: bool) {
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
                    // The reserved thread never touches the low queue, so it
                    // is always free for the next thing the user asks for.
                    let next = if interactive {
                        q.high.pop_front()
                    } else {
                        q.high.pop_front().or_else(|| q.low.pop_front())
                    };
                    if let Some(job) = next {
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
        Job::Thumbs {
            clip_id,
            path,
            duration_frames,
            fps,
            tiles,
            still,
            cache_dir,
        } => {
            let result = match &shared.tools.ffmpeg {
                Some(ffmpeg) => make_sheet(
                    ffmpeg,
                    &path,
                    duration_frames,
                    fps,
                    if still { 1 } else { tiles },
                    still,
                    cache_dir.as_deref(),
                ),
                None => Err(anyhow::anyhow!("ffmpeg is not available")),
            };
            JobResult::Thumbs { clip_id, result }
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
                        still: false,
                        path: source,
                        proxy_path: None,
                        duration_frames: info.native_frames,
                        native_frames: info.native_frames,
                        native_fps_num: info.fps.num,
                        native_fps_den: info.fps.den,
                        width: info.width,
                        height: info.height,
                        sample_aspect_num: info.sample_aspect_num,
                        sample_aspect_den: info.sample_aspect_den,
                        progressive: info.progressive,
                        colorspace: info.colorspace,
                        has_audio: info.has_audio,
                        video_index: info.video_index,
                        audio_index: info.audio_index,
                        mark_in: None,
                        mark_out: None,
                        rate_mismatch: false,
                        variable_rate: false,
                        flagged: false,
                    };
                    proxy::generate(ffmpeg, ffprobe, &stub, &info, &proxy_dir)
                }
                _ => Err(anyhow::anyhow!(
                    "proxy generation needs both ffmpeg and ffprobe"
                )),
            };
            JobResult::ProxyDone { clip_id, result }
        }
        Job::Rotate {
            clip_id,
            path,
            turn,
        } => {
            let result = match (&shared.tools.ffmpeg, &shared.tools.ffprobe) {
                (Some(ffmpeg), Some(ffprobe)) => {
                    rotate::rotate_in_place(ffmpeg, ffprobe, &path, turn)
                }
                _ => Err(anyhow::anyhow!("rotating needs both ffmpeg and ffprobe")),
            };
            JobResult::Rotated { clip_id, result }
        }
        Job::Waveform {
            clip_id,
            path,
            cache_dir,
        } => {
            let result = match &shared.tools.ffmpeg {
                Some(ffmpeg) => make_waveform(ffmpeg, &path, cache_dir.as_deref()),
                None => Err(anyhow::anyhow!("ffmpeg is not available")),
            };
            JobResult::Waved { clip_id, result }
        }
    }
}

/// A clip's audio envelope, kept on disk between sessions.
///
/// Reading the audio of a long clip is seconds of ffmpeg; the result is two
/// kilobytes. Nothing else in the application has a ratio like that, so this
/// is cached even more eagerly than the filmstrips are.
fn make_waveform(
    ffmpeg: &std::path::Path,
    path: &std::path::Path,
    cache_dir: Option<&std::path::Path>,
) -> Result<Vec<u8>> {
    let buckets = waveform::BUCKETS;
    let cache_file = cache_dir.map(|d| {
        d.join(format!(
            "{:016x}-{buckets}.peaks",
            fingerprint(path, &[buckets as i64])
        ))
    });
    if let Some(file) = &cache_file {
        if let Ok(bytes) = std::fs::read(file) {
            if bytes.len() == buckets {
                return Ok(bytes);
            }
        }
    }

    let peaks = waveform::extract(ffmpeg, path, buckets)?;
    if let Some(file) = &cache_file {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(file, &peaks) {
            log::debug!("cannot cache peaks at {}: {e}", file.display());
        }
    }
    Ok(peaks)
}

/// One frame, scaled down, straight out of ffmpeg as a PNG on stdout.
/// The frame sampled for tile `i`: the centre of the slice it represents, so
/// tile 0 is not frame 0 — the first frame of a shot is very often black.
pub fn filmstrip_frame(tile: usize, duration_frames: i64, tiles: usize) -> i64 {
    let last = (duration_frames - 1).max(0);
    let n = tiles.max(1) as i64;
    ((tile as i64 * 2 + 1) * last) / (n * 2)
}

/// Which tile best represents `frame`. The inverse of `filmstrip_frame`, used
/// to paint timeline blocks with the picture at that point in the clip.
pub fn tile_for_frame(frame: i64, duration_frames: i64, tiles: usize) -> usize {
    let tiles = tiles.max(1);
    let last = (duration_frames - 1).max(1);
    let t = (frame.clamp(0, last) as f64) / last as f64;
    ((t * tiles as f64) as usize).min(tiles - 1)
}

/// Identifies a file, the version of it on disk right now, and whatever else
/// would change what is derived from it.
///
/// A rotated or replaced source therefore misses rather than serving a stale
/// strip or a waveform of the wrong audio, and no explicit invalidation is
/// needed anywhere.
fn fingerprint(path: &std::path::Path, extra: &[i64]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    extra.hash(&mut h);
    if let Ok(meta) = std::fs::metadata(path) {
        meta.len().hash(&mut h);
        if let Ok(t) = meta.modified() {
            if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                d.as_nanos().hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Identifies a cached sheet.
fn cache_key(path: &std::path::Path, duration_frames: i64, tiles: usize) -> String {
    format!(
        "{:016x}",
        fingerprint(
            path,
            &[duration_frames, tiles as i64, i64::from(THUMB_WIDTH)],
        )
    )
}

fn read_cached(file: &std::path::Path, tiles: usize) -> Option<Sheet> {
    let img = image::open(file).ok()?.to_rgba8();
    let (cols, rows) = grid_for(tiles);
    Some(Sheet {
        width: img.width() as usize,
        height: img.height() as usize,
        rgba: img.into_raw(),
        cols,
        rows,
        tiles,
    })
}

/// A clip's tiles, packed into one image.
///
/// Kept on disk between sessions. Reopening a project used to re-extract every
/// thumbnail from scratch, which on a large bin is minutes of ffmpeg before
/// any picture appears; now it is a file read.
fn make_sheet(
    ffmpeg: &std::path::Path,
    path: &std::path::Path,
    duration_frames: i64,
    fps: Rational,
    tiles: usize,
    still: bool,
    cache_dir: Option<&std::path::Path>,
) -> Result<Sheet> {
    let tiles = tiles.max(1);
    let cache_file =
        cache_dir.map(|d| d.join(format!("{}.png", cache_key(path, duration_frames, tiles))));
    if let Some(file) = &cache_file {
        if let Some(hit) = read_cached(file, tiles) {
            return Ok(hit);
        }
    }

    // A photograph holds one frame, at the start, however long the project
    // says it lasts. Sampling the middle of its invented duration — which is
    // what every other clip wants — asks ffmpeg to seek thirty seconds into a
    // single image, and it answers with nothing at all, silently and with a
    // successful exit code.
    let frames: Vec<i64> = if still {
        vec![0]
    } else {
        (0..tiles)
            .map(|i| filmstrip_frame(i, duration_frames, tiles))
            .collect()
    };

    // Each batch is one ffmpeg run producing a horizontal strip; the strips
    // are then cut up and packed into the grid. One launch per tile — which is
    // what this used to do — costs more in process startup than the decoding.
    let mut strips: Vec<(usize, image::RgbaImage)> = Vec::new();
    for (b, chunk) in frames.chunks(BATCH).enumerate() {
        match grab_strip(ffmpeg, path, chunk, fps) {
            Ok(img) => strips.push((b * BATCH, img)),
            // Odd files exist, and a seek that lands nowhere makes `hstack`
            // fail for the whole batch. Falling back frame by frame is slow
            // but tolerates individual gaps, so a difficult clip still gets
            // thumbnails rather than none.
            Err(e) => {
                log::debug!("batch {b} of {} failed: {e:#}", path.display());
                for (k, &f) in chunk.iter().enumerate() {
                    if let Ok(img) = grab_frame(ffmpeg, path, f, fps) {
                        strips.push((b * BATCH + k, img));
                    }
                }
            }
        }
    }

    let sheet = pack(&strips, tiles)?;
    if let Some(file) = &cache_file {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = sheet.save(file) {
            log::debug!("cannot cache sheet at {}: {e}", file.display());
        }
    }

    let (cols, rows) = grid_for(tiles);
    Ok(Sheet {
        width: sheet.width() as usize,
        height: sheet.height() as usize,
        rgba: sheet.into_raw(),
        cols,
        rows,
        tiles,
    })
}

/// Cut the strips into tiles and lay them out row-major.
///
/// Each entry is a horizontal run of tiles starting at a known index, which is
/// what both the batched and the frame-by-frame paths produce.
fn pack(strips: &[(usize, image::RgbaImage)], tiles: usize) -> Result<image::RgbaImage> {
    let (_, first) = strips
        .first()
        .context("ffmpeg produced no frames for this clip")?;
    // Every tile in a strip is the same width by construction.
    let n = (first.width() as usize / THUMB_WIDTH as usize).max(1);
    let (tw, th) = ((first.width() / n as u32).max(1), first.height());

    let (cols, rows) = grid_for(tiles);
    let mut sheet = image::RgbaImage::new(tw * cols as u32, th * rows as u32);
    for (base, img) in strips {
        if img.height() != th {
            continue;
        }
        let count = (img.width() / tw) as usize;
        for k in 0..count {
            let index = base + k;
            if index >= tiles {
                break;
            }
            let (dx, dy) = ((index % cols) as u32 * tw, (index / cols) as u32 * th);
            for y in 0..th {
                for x in 0..tw {
                    sheet.put_pixel(dx + x, dy + y, *img.get_pixel(k as u32 * tw + x, y));
                }
            }
        }
    }
    Ok(sheet)
}

/// One ffmpeg run: a fast input seek per frame, scaled and stacked side by
/// side by the filter graph.
fn grab_strip(
    ffmpeg: &std::path::Path,
    path: &std::path::Path,
    frames: &[i64],
    fps: Rational,
) -> Result<image::RgbaImage> {
    if frames.len() == 1 {
        return grab_frame(ffmpeg, path, frames[0], fps);
    }
    let n = frames.len();
    let mut cmd = quiet_command(ffmpeg);
    cmd.args(["-v", "error"]);
    for &frame in frames {
        // Input seek (`-ss` before `-i`) is orders of magnitude faster than
        // output seek and is accurate enough for a bin thumbnail.
        cmd.arg("-ss")
            .arg(format!("{:.6}", frame_to_seconds(frame, fps)))
            .arg("-i")
            .arg(path);
    }

    let mut filter = String::new();
    for i in 0..n {
        // `setsar=1` so anamorphic sources cannot make the tiles disagree
        // about shape, which `hstack` refuses to stack.
        filter.push_str(&format!(
            "[{i}:v]scale={THUMB_WIDTH}:-2:flags=fast_bilinear,setsar=1[t{i}];"
        ));
    }
    for i in 0..n {
        filter.push_str(&format!("[t{i}]"));
    }
    filter.push_str(&format!("hstack=inputs={n}[o]"));

    cmd.args(["-filter_complex", &filter])
        .args(["-map", "[o]", "-frames:v", "1"])
        .args(["-f", "image2pipe", "-vcodec", "png", "-"]);

    let output = cmd
        .output()
        .with_context(|| format!("cannot run ffmpeg at {}", ffmpeg.display()))?;
    if !output.status.success() || output.stdout.is_empty() {
        bail!(
            "ffmpeg produced no strip: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(
        image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Png)
            .context("cannot decode the strip ffmpeg produced")?
            .to_rgba8(),
    )
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
        let frames: Vec<i64> = (0..SCRUB_TILES)
            .map(|i| filmstrip_frame(i, 300, SCRUB_TILES))
            .collect();
        assert_eq!(frames.len(), SCRUB_TILES);
        // Never the very first frame, which is usually black.
        assert!(frames[0] > 0, "tile 0 sampled frame {}", frames[0]);
        assert!(frames.windows(2).all(|w| w[0] <= w[1]), "{frames:?}");
        assert!(*frames.last().unwrap() <= last);
    }

    #[test]
    fn tile_lookup_inverts_tile_sampling() {
        // Every tile's own sample frame must map back to that tile, at any
        // sheet size — the poster has one tile, a scrub sheet has 112.
        for tiles in [1, 12, SCRUB_TILES] {
            // Long enough that each tile gets a distinct frame.
            let dur = 4000;
            for i in 0..tiles {
                let f = filmstrip_frame(i, dur, tiles);
                assert_eq!(tile_for_frame(f, dur, tiles), i, "tiles {tiles}, tile {i}");
            }
            // Ends clamp rather than running off either side.
            assert_eq!(tile_for_frame(-5, dur, tiles), 0);
            assert_eq!(tile_for_frame(99_999, dur, tiles), tiles - 1);
        }
    }

    #[test]
    fn filmstrip_handles_degenerate_clips() {
        // A one-frame clip: every tile is frame 0, and nothing panics.
        for i in 0..SCRUB_TILES {
            assert_eq!(filmstrip_frame(i, 1, SCRUB_TILES), 0);
            assert_eq!(filmstrip_frame(i, 0, SCRUB_TILES), 0);
        }
        assert_eq!(tile_for_frame(0, 0, 1), 0);
    }

    #[test]
    fn a_sheet_grid_holds_every_tile_and_stays_squarish() {
        for tiles in [1usize, 2, 12, SCRUB_TILES] {
            let (cols, rows) = grid_for(tiles);
            assert!(cols * rows >= tiles, "{tiles} does not fit {cols}x{rows}");
            assert!(cols <= SHEET_COLS.max(1));
            // A single row of 112 tiles would be 10752px wide, which is what
            // the grid exists to avoid.
            assert!(cols * THUMB_WIDTH as usize <= 4096);
        }
        assert_eq!(grid_for(1), (1, 1));
    }

    #[test]
    fn the_poster_outranks_every_sheet() {
        // The bin is unusable until its pictures appear, so a one-tile poster
        // must never queue behind another clip's 112-tile sheet.
        let poster = Job::Thumbs {
            clip_id: ClipId::new(),
            path: PathBuf::from("a.mp4"),
            duration_frames: 100,
            fps: Rational::new(30, 1),
            tiles: 1,
            still: false,
            cache_dir: None,
        };
        let sheet = Job::Thumbs {
            clip_id: ClipId::new(),
            path: PathBuf::from("a.mp4"),
            duration_frames: 100,
            fps: Rational::new(30, 1),
            tiles: SCRUB_TILES,
            still: false,
            cache_dir: None,
        };
        assert_eq!(poster.priority(), Priority::High);
        assert_eq!(sheet.priority(), Priority::Low);
        assert_eq!(
            Job::Probe { path: PathBuf::from("a.mp4") }.priority(),
            Priority::High
        );
    }

    /// A waveform is asked for about the clip on screen right now, so it must
    /// never queue behind the sheets and proxies of clips nobody is looking
    /// at.
    #[test]
    fn a_waveform_does_not_queue_behind_background_work() {
        let wave = Job::Waveform {
            clip_id: ClipId::new(),
            path: PathBuf::from("a.mp4"),
            cache_dir: None,
        };
        assert_eq!(wave.priority(), Priority::High);
    }

    /// The second look at a clip must not decode its audio again: that is the
    /// whole reason these are written to disk.
    #[test]
    fn a_waveform_is_served_from_the_cache_the_second_time() {
        let Some(ffmpeg) = Tools::discover().ffmpeg else {
            eprintln!("SKIPPED: ffmpeg is required");
            return;
        };
        let dir = std::env::temp_dir().join("roughcut-wavecache-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("tone.mp4");
        let ok = quiet_command(&ffmpeg)
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=30:d=2"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
            .args(["-c:a", "aac", "-shortest"])
            .arg(&clip)
            .status()
            .expect("cannot run ffmpeg")
            .success();
        assert!(ok);

        let first = make_waveform(&ffmpeg, &clip, Some(&dir)).expect("no peaks");
        assert_eq!(first.len(), waveform::BUCKETS);

        // Take ffmpeg away entirely. A cache hit cannot need it; a miss would
        // fail outright, which is exactly the distinction being tested.
        let second = make_waveform(std::path::Path::new("no-such-ffmpeg"), &clip, Some(&dir))
            .expect("the second call did not come from the cache");
        assert_eq!(first, second);

        // A file that changed underneath us is a different fingerprint, so the
        // stale peaks are never served.
        std::fs::write(&clip, b"not a video any more").unwrap();
        assert!(make_waveform(std::path::Path::new("no-such-ffmpeg"), &clip, Some(&dir)).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A photograph gets a picture in the bin.
    ///
    /// It did not, and the way it failed is the interesting part: a still is
    /// given an artificial minute of duration, the poster is sampled from the
    /// middle of a clip, and asking ffmpeg for the frame thirty seconds into a
    /// single image produces an empty stream and exit code zero. Nothing
    /// errored; the bin simply stayed grey.
    #[test]
    fn a_photograph_gets_a_poster_from_its_only_frame() {
        let Some(ffmpeg) = Tools::discover().ffmpeg else {
            eprintln!("SKIPPED: ffmpeg is required");
            return;
        };
        let dir = std::env::temp_dir().join("roughcut-still-poster-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("photo.png");
        assert!(quiet_command(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x480:d=1"])
            .args(["-frames:v", "1"])
            .arg(&png)
            .status()
            .expect("cannot run ffmpeg")
            .success());

        // An hour of invented duration, exactly as a real still is given.
        let sheet = make_sheet(&ffmpeg, &png, 1800, Rational::new(30, 1), 1, true, None)
            .expect("a photograph must produce a poster");
        assert_eq!(sheet.tiles, 1);
        assert!(sheet.width > 0 && sheet.height > 0);
        assert!(
            sheet.rgba.iter().any(|&b| b != 0),
            "the poster came back blank"
        );

        // And the same call without the flag is the bug, still reproducible.
        assert!(
            make_sheet(&ffmpeg, &png, 1800, Rational::new(30, 1), 1, false, None).is_err(),
            "seeking into a single image should fail rather than quietly work"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pool_size_is_bounded() {
        let n = pool_size();
        assert!((1..=4).contains(&n), "pool size {n} outside the §3 bound");
        assert!(n <= (num_cpus::get() / 2).max(1));
    }
}
