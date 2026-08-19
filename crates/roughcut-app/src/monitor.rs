//! The playback controller.
//!
//! The application owns an `i64` frame number and this module makes mpv agree
//! with it. mpv's `time-pos` is read back only to *report* progress during
//! playback; a cut point is never derived from it directly. Every seek is
//! computed from the integer frame with `seek <t> absolute+exact`.

use anyhow::Result;
use roughcut_core::time::{frame_to_seek_seconds, seconds_to_frame, Rational};
use roughcut_mpv::{player::Event, MpvLib, Player};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Speeds `J` and `L` cycle through.
pub const SHUTTLE_SPEEDS: [u32; 4] = [1, 2, 4, 8];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Paused,
    Forward(u32),
    /// Reverse is driven by repeated exact backward seeks on a timer, because
    /// negative playback rates in mpv are unreliable above 1x.
    Reverse(u32),
}

impl Transport {
    pub fn is_playing(self) -> bool {
        !matches!(self, Self::Paused)
    }

    /// Next speed in the 1 -> 2 -> 4 -> 8 cycle, wrapping back to 1.
    fn next_speed(current: u32) -> u32 {
        let i = SHUTTLE_SPEEDS.iter().position(|&s| s == current);
        match i {
            Some(i) if i + 1 < SHUTTLE_SPEEDS.len() => SHUTTLE_SPEEDS[i + 1],
            Some(_) => SHUTTLE_SPEEDS[0],
            None => SHUTTLE_SPEEDS[0],
        }
    }
}

/// An in-flight position change. The two kinds finish on different mpv
/// signals, which is why they are distinguished.
#[derive(Debug, Clone, Copy)]
enum Pending {
    /// `seek absolute+exact`. mpv sets `time-pos` to the target the instant
    /// the seek is issued, so only `PLAYBACK_RESTART` means "the frame is
    /// ready".
    Seek { at: Instant, frame: i64 },
    /// `frame-step` / `frame-back-step`. These do not necessarily emit
    /// `PLAYBACK_RESTART`, so completion is taken from `time-pos` reaching
    /// the target — which for a step is an honest signal, because mpv only
    /// reports the new position once the frame has been decoded.
    Step { at: Instant, frame: i64 },
}

pub struct Monitor {
    lib: Option<Arc<MpvLib>>,
    player: Option<Player>,
    /// Set once if libmpv could not be loaded; reported in the status bar.
    pub load_error: Option<String>,
    /// File currently loaded into mpv.
    loaded: Option<PathBuf>,
    /// Seek to apply as soon as the file finishes loading.
    pending_seek: Option<i64>,
    /// Last frame we asked mpv to display.
    requested_frame: Option<i64>,
    /// Frame mpv most recently reported, derived from `time-pos`.
    pub reported_frame: Option<i64>,
    pub transport: Transport,
    pub hwdec: Option<String>,
    /// mpv has reached the end of the loaded file. Reported, not acted on:
    /// only the application knows whether that means "stop" or "roll onto the
    /// next clip on the timeline".
    pub eof: bool,
    /// Timer for reverse shuttle.
    last_reverse_tick: Option<Instant>,
    /// The outstanding position change, if any. Used both to know when mpv
    /// has caught up and to report real latency against §3's budgets; set
    /// `RUST_LOG=roughcut=debug` to see the numbers.
    pending: Option<Pending>,
    /// Target we have already issued a corrective seek for, so a frame that
    /// stubbornly refuses to match cannot start a seek loop.
    corrected_for: Option<i64>,
    /// Rolling record of the last few latencies, newest last.
    pub seek_latencies_ms: Vec<f64>,
    fps: Rational,
    volume: f64,
    /// Repaint hook handed to mpv's wakeup and render-update callbacks.
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Monitor {
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            lib: None,
            player: None,
            load_error: None,
            loaded: None,
            pending_seek: None,
            requested_frame: None,
            reported_frame: None,
            transport: Transport::Paused,
            hwdec: None,
            eof: false,
            last_reverse_tick: None,
            pending: None,
            corrected_for: None,
            seek_latencies_ms: Vec::new(),
            fps: Rational::new(30000, 1001),
            volume: 80.0,
            repaint,
        }
    }

    pub fn set_fps(&mut self, fps: Rational) {
        self.fps = fps;
    }

    pub fn player(&self) -> Option<&Player> {
        self.player.as_ref()
    }

    pub fn is_available(&self) -> bool {
        self.player.is_some()
    }

    pub fn library_path(&self) -> Option<&Path> {
        self.lib.as_ref().map(|l| l.path.as_path())
    }

    /// Load libmpv and create the player. Deferred until a clip is actually
    /// opened so that cold start stays under the 1 s budget — libmpv is a
    /// large library and mapping it is not free.
    pub fn ensure_started(&mut self, explicit: Option<&Path>) -> bool {
        if self.player.is_some() {
            return true;
        }
        if self.load_error.is_some() {
            return false;
        }
        let lib = match explicit {
            Some(p) => MpvLib::load_from(p),
            None => MpvLib::load(),
        };
        let lib = match lib {
            Ok(l) => Arc::new(l),
            Err(e) => {
                log::error!("libmpv: {e:#}");
                self.load_error = Some(format!("{e}"));
                return false;
            }
        };
        let (major, minor) = lib.api_version();
        log::info!(
            "libmpv {}.{} loaded from {}",
            major,
            minor,
            lib.path.display()
        );

        match Player::new(lib.clone()) {
            Ok(mut p) => {
                p.set_wakeup_callback(self.repaint.clone());
                let _ = p.set_volume(self.volume);
                self.player = Some(p);
                self.lib = Some(lib);
                true
            }
            Err(e) => {
                log::error!("mpv player: {e:#}");
                self.load_error = Some(format!("{e}"));
                false
            }
        }
    }

    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume;
        if let Some(p) = &self.player {
            let _ = p.set_volume(volume);
        }
    }

    /// Drain mpv's event queue. Returns true if anything worth repainting for
    /// happened.
    pub fn pump_events(&mut self) -> bool {
        // Drain first, then handle: the handlers need `&mut self`, which
        // cannot coexist with the borrow of `self.player` that polling needs.
        let events: Vec<Event> = {
            let Some(player) = &self.player else {
                return false;
            };
            let mut events = Vec::new();
            while let Some(ev) = player.poll_event() {
                events.push(ev);
            }
            events
        };
        if events.is_empty() {
            return false;
        }

        let changed = true;
        for ev in events {
            match ev {
                Event::FileLoaded => {
                    self.eof = false;
                    self.hwdec = self.player.as_ref().and_then(|p| p.hwdec_active());
                    match &self.hwdec {
                        Some(h) => log::info!("hardware decode active: {h}"),
                        // §3 makes hardware decode mandatory rather than
                        // opportunistic, so falling back is worth saying out
                        // loud as well as showing in the alert bar.
                        None => log::warn!("hardware decode unavailable — decoding in software"),
                    }
                    let resume = matches!(self.transport, Transport::Forward(_));
                    let seek_to = self.pending_seek.take();
                    let fps = self.fps;
                    if let Some(player) = &self.player {
                        if let Some(frame) = seek_to {
                            let _ = player.seek_exact(frame_to_seek_seconds(frame, fps));
                        }
                        // Resume only after the file is really open, otherwise
                        // the first frames of a clip can be skipped.
                        let _ = player.set_paused(!resume);
                    }
                }
                Event::TimePos(Some(t)) => {
                    let frame = seconds_to_frame(t, self.fps);
                    self.reported_frame = Some(frame);
                    if let Some(Pending::Step { at, frame: want }) = self.pending {
                        if frame == want {
                            self.finish_pending(at, want, "step");
                        }
                    }
                }
                Event::TimePos(None) => {}
                Event::PlaybackRestart => {
                    if let Some(Pending::Seek { at, frame }) = self.pending {
                        self.finish_pending(at, frame, "seek");
                    }
                }
                // Deliberately does not pause. Pausing here is what stopped
                // timeline playback dead at the end of any clip whose out
                // point was the last frame of its source file.
                Event::EofReached(reached) => self.eof = reached,
                Event::HwdecCurrent(h) => {
                    self.hwdec = h.filter(|s| s != "no" && !s.is_empty());
                }
                Event::LogMessage { level, text } => {
                    log::debug!("mpv[{level}] {text}");
                }
                Event::Shutdown => {
                    self.player = None;
                    self.load_error = Some("mpv shut down unexpectedly".into());
                    return true;
                }
                _ => {}
            }
        }
        changed
    }

    /// Make mpv show `frame` of `path`. Called every update; it only issues a
    /// command when something actually needs to change.
    pub fn show(&mut self, path: &Path, frame: i64) {
        self.show_impl(path, frame, false);
    }

    /// Move even while playing forward.
    ///
    /// `show` leaves mpv alone during playback, because seeking every frame
    /// would fight it. That is wrong at exactly one moment: when the
    /// application crosses a cut and mpv's own progress is no longer the right
    /// answer — especially when the next clip comes from the same file, where
    /// there is no load to force the issue.
    pub fn jump(&mut self, path: &Path, frame: i64) {
        self.eof = false;
        self.requested_frame = None;
        self.corrected_for = None;
        self.show_impl(path, frame, true);
    }

    /// Forget which file is loaded, so the next `show` opens it afresh.
    ///
    /// Needed when the bytes behind a path have changed underneath mpv — the
    /// only case being a clip Roughcut has just rotated on disk.
    pub fn reload(&mut self) {
        self.loaded = None;
        self.eof = false;
        self.requested_frame = None;
        self.corrected_for = None;
    }

    fn show_impl(&mut self, path: &Path, frame: i64, force: bool) {
        let Some(player) = &self.player else {
            return;
        };

        let needs_load = self.loaded.as_deref() != Some(path);
        if needs_load {
            if let Err(e) = player.load_file(path) {
                log::warn!("cannot open {}: {e}", path.display());
                return;
            }
            self.loaded = Some(path.to_path_buf());
            self.pending_seek = Some(frame);
            self.requested_frame = Some(frame);
            self.reported_frame = None;
            return;
        }

        // While playing forward, mpv owns the position; seeking every frame
        // would fight it and stutter.
        if !force && matches!(self.transport, Transport::Forward(_)) {
            return;
        }
        if self.pending_seek.is_some() {
            self.pending_seek = Some(frame);
            self.requested_frame = Some(frame);
            return;
        }

        // At most one seek in flight.
        //
        // Dragging the scrub bar produces a new target every pixel. Firing a
        // seek for each one buries mpv in work it will never finish, and
        // because it shares the GL context with the UI, everything on screen
        // ends up moving at the rate frames come out of the decoder — the
        // playhead included, which should never wait for a picture. Dropping
        // the intermediate targets costs nothing: the next pass asks for
        // wherever the pointer is by then, which is the only position that was
        // ever wanted.
        if self.pending.is_some() && !force {
            return;
        }

        if self.requested_frame == Some(frame) {
            // Already asked for this one. Once mpv has settled, check it
            // really landed where we asked — a step is only worth using if it
            // is verifiably exact — and correct it once if not.
            if self.pending.is_none() && self.corrected_for != Some(frame) {
                if let Some(reported) = self.reported_frame {
                    if reported != frame {
                        log::debug!(
                            "position drifted: mpv is on {reported}, wanted {frame} — correcting"
                        );
                        self.corrected_for = Some(frame);
                        self.pending = Some(Pending::Seek {
                            at: Instant::now(),
                            frame,
                        });
                        let _ = player.seek_exact(frame_to_seek_seconds(frame, self.fps));
                    }
                }
            }
            return;
        }

        let delta = self.requested_frame.map(|prev| frame - prev);
        self.requested_frame = Some(frame);
        self.corrected_for = None;
        let now = Instant::now();

        // A neighbouring frame is reached with mpv's frame stepping, which
        // decodes one frame. An arbitrary position needs a real seek.
        //
        // §5 rule 6 describes stepping as "increment the integer counter and
        // seek". The counter is still the sole source of truth here — that is
        // the part that matters — but issuing an absolute seek for every
        // single-frame step costs a full re-decode from the start of the GOP,
        // measured at 120-180 ms and worsening across the GOP on 4K H.264.
        // That blows §3's 33 ms stepping budget outright. Frame stepping
        // reaches the identical frame in a few milliseconds, and the check
        // above catches it if it ever does not.
        // Only forward stepping gets the fast path. mpv's `frame-back-step`
        // relies on a backward decode cache that `cache=no` disables, and it
        // was measured to be a silent no-op here — stepping backwards seeks.
        match delta {
            Some(1) => {
                self.pending = Some(Pending::Step { at: now, frame });
                let _ = player.frame_step();
            }
            _ => {
                self.pending = Some(Pending::Seek { at: now, frame });
                let _ = player.seek_exact(frame_to_seek_seconds(frame, self.fps));
            }
        }
    }

    fn finish_pending(&mut self, at: Instant, frame: i64, kind: &str) {
        self.pending = None;
        let ms = at.elapsed().as_secs_f64() * 1000.0;
        log::debug!("{kind} to frame {frame} settled in {ms:.1} ms");
        self.seek_latencies_ms.push(ms);
        if self.seek_latencies_ms.len() > 256 {
            self.seek_latencies_ms.remove(0);
        }
    }

    /// Unload whatever is playing (nothing selected).
    pub fn clear(&mut self) {
        if self.loaded.is_none() {
            return;
        }
        if let Some(p) = &self.player {
            let _ = p.stop();
        }
        self.loaded = None;
        self.pending_seek = None;
        self.requested_frame = None;
        self.reported_frame = None;
        self.transport = Transport::Paused;
    }

    // --- transport ----------------------------------------------------------

    pub fn toggle_play(&mut self) {
        match self.transport {
            Transport::Paused => self.play_forward_1x(),
            _ => self.pause(),
        }
    }

    fn play_forward_1x(&mut self) {
        self.set_transport(Transport::Forward(1));
    }

    /// `L` — play forward, cycling 1x, 2x, 4x, 8x on repeated presses.
    pub fn shuttle_forward(&mut self) {
        let next = match self.transport {
            Transport::Forward(s) => Transport::next_speed(s),
            _ => 1,
        };
        self.set_transport(Transport::Forward(next));
    }

    /// `J` — play backward, cycling 1x, 2x, 4x, 8x.
    pub fn shuttle_reverse(&mut self) {
        let next = match self.transport {
            Transport::Reverse(s) => Transport::next_speed(s),
            _ => 1,
        };
        self.set_transport(Transport::Reverse(next));
    }

    pub fn pause(&mut self) {
        self.set_transport(Transport::Paused);
    }

    fn set_transport(&mut self, t: Transport) {
        // Every arrow key pauses before stepping. Without this guard the
        // "already paused" case would still run the arm below, and clearing
        // `requested_frame` there is what previously stopped single-frame
        // stepping from ever recognising a +1 move.
        if self.transport == t {
            return;
        }
        self.transport = t;
        self.last_reverse_tick = None;
        let Some(player) = &self.player else { return };
        match t {
            Transport::Paused => {
                let _ = player.set_paused(true);
                let _ = player.set_speed(1.0);
                // Coming out of playback, mpv is wherever it got to; adopt
                // that as the current request so the next step is relative to
                // reality rather than to a stale target.
                self.requested_frame = self.reported_frame;
            }
            Transport::Forward(s) => {
                let _ = player.set_speed(s as f64);
                let _ = player.set_paused(false);
            }
            Transport::Reverse(_) => {
                // Reverse is seek-driven, so mpv itself stays paused.
                let _ = player.set_speed(1.0);
                let _ = player.set_paused(true);
                self.last_reverse_tick = Some(Instant::now());
            }
        }
    }

    /// How many frames reverse shuttle should move this update, if the timer
    /// has elapsed. Returns 0 when it is not time yet or we are not reversing.
    pub fn reverse_step_due(&mut self) -> i64 {
        let Transport::Reverse(speed) = self.transport else {
            return 0;
        };
        let period = self.frame_period();
        let now = Instant::now();
        let last = self.last_reverse_tick.get_or_insert(now);
        let elapsed = now.saturating_duration_since(*last);
        if elapsed < period {
            return 0;
        }
        // Catch up if the UI was busy, but never overshoot wildly.
        let ticks = (elapsed.as_secs_f64() / period.as_secs_f64()).floor() as i64;
        let ticks = ticks.clamp(1, 4);
        *last = now;
        ticks * speed as i64
    }

    /// One frame of wall-clock time at the project rate.
    pub fn frame_period(&self) -> Duration {
        let fps = self.fps.as_f64().max(1.0);
        Duration::from_secs_f64(1.0 / fps)
    }

    /// While playing, the UI must be repainted at the frame rate. While
    /// paused, it must not be repainted at all.
    pub fn repaint_interval(&self) -> Option<Duration> {
        self.transport.is_playing().then(|| self.frame_period())
    }

    /// Position mpv is actually showing, for the source monitor during
    /// playback. `None` before the first `time-pos` arrives.
    pub fn playback_frame(&self) -> Option<i64> {
        self.reported_frame
    }

    /// A seek is still in flight, so what mpv reports right now is where it
    /// was, not where it is going, and the picture on screen is the old one.
    ///
    /// Steps are excluded: they land within a frame or two, and treating them
    /// as "in flight" would make every arrow key flash a placeholder.
    ///
    /// Bounded in time deliberately. A seek that never completes — a damaged
    /// file, a stream that will not restart — would otherwise leave playback
    /// permanently unable to report progress, which looks exactly like the
    /// application having frozen.
    pub fn is_seeking(&self) -> bool {
        match self.pending {
            Some(Pending::Seek { at, .. }) => at.elapsed() < Duration::from_millis(500),
            Some(Pending::Step { .. }) | None => false,
        }
    }

    /// Median / p95 / max seek latency in milliseconds, for checking §3's
    /// "keypress to visible frame change" and "seek to arbitrary frame"
    /// budgets against reality.
    pub fn seek_latency_summary(&self) -> Option<(usize, f64, f64, f64)> {
        if self.seek_latencies_ms.is_empty() {
            return None;
        }
        let mut s = self.seek_latencies_ms.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = s.len();
        let median = s[n / 2];
        let p95 = s[(n * 95 / 100).min(n - 1)];
        let max = *s.last().unwrap();
        Some((n, median, p95, max))
    }

    pub fn shutdown(&mut self) -> Result<()> {
        if let Some((n, median, p95, max)) = self.seek_latency_summary() {
            log::info!(
                "seek latency over {n} seeks: median {median:.1} ms, \
                 p95 {p95:.1} ms, max {max:.1} ms"
            );
        }
        self.player = None;
        self.lib = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuttle_speeds_cycle_and_wrap() {
        assert_eq!(Transport::next_speed(1), 2);
        assert_eq!(Transport::next_speed(2), 4);
        assert_eq!(Transport::next_speed(4), 8);
        assert_eq!(Transport::next_speed(8), 1);
    }

    #[test]
    fn transport_reports_playing_state() {
        assert!(!Transport::Paused.is_playing());
        assert!(Transport::Forward(1).is_playing());
        assert!(Transport::Reverse(4).is_playing());
    }
}
