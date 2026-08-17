//! Timeline arithmetic and edit operations.
//!
//! The timeline is a flat ordered `Vec<TimelineItem>`. A clip's start position
//! is the cumulative sum of the durations before it — computed, never stored —
//! so ripple operations are just `Vec` insert/remove and cannot desynchronise.

use crate::model::{ClipId, Project, TimelineItem};

/// Total length of the assembled sequence, in frames.
pub fn total_frames(timeline: &[TimelineItem]) -> i64 {
    timeline.iter().map(|i| i.len()).sum()
}

/// Frame at which `index` starts. `index == len` gives the end of the timeline.
pub fn item_start(timeline: &[TimelineItem], index: usize) -> i64 {
    timeline.iter().take(index).map(|i| i.len()).sum()
}

/// The item containing `frame`, plus the offset of `frame` within it.
/// Returns `None` for a frame at or past the end.
pub fn item_at(timeline: &[TimelineItem], frame: i64) -> Option<(usize, i64)> {
    if frame < 0 {
        return None;
    }
    let mut acc = 0i64;
    for (i, item) in timeline.iter().enumerate() {
        let len = item.len();
        if frame < acc + len {
            return Some((i, frame - acc));
        }
        acc += len;
    }
    None
}

/// Every boundary on the timeline: 0, each clip start, and the end.
/// Sorted and deduplicated.
pub fn cut_points(timeline: &[TimelineItem]) -> Vec<i64> {
    let mut points = Vec::with_capacity(timeline.len() + 2);
    let mut acc = 0i64;
    points.push(0);
    for item in timeline {
        acc += item.len();
        points.push(acc);
    }
    points.dedup();
    points
}

/// Nearest cut point strictly before `frame`.
pub fn prev_cut(timeline: &[TimelineItem], frame: i64) -> Option<i64> {
    cut_points(timeline).into_iter().filter(|&p| p < frame).next_back()
}

/// Nearest cut point strictly after `frame`.
pub fn next_cut(timeline: &[TimelineItem], frame: i64) -> Option<i64> {
    cut_points(timeline).into_iter().find(|&p| p > frame)
}

/// Highest legal playhead position. One past the last frame is *not* legal;
/// an empty timeline pins the playhead at 0.
pub fn last_frame(timeline: &[TimelineItem]) -> i64 {
    (total_frames(timeline) - 1).max(0)
}

// ---------------------------------------------------------------------------
// Edit operations. Each returns whether it changed anything, so callers can
// avoid pushing a pointless undo snapshot.
// ---------------------------------------------------------------------------

/// Append a marked source range to the end of the timeline.
pub fn append(project: &mut Project, clip_id: ClipId, in_frame: i64, out_frame: i64) -> bool {
    let Some(item) = make_item(project, clip_id, in_frame, out_frame) else {
        return false;
    };
    project.timeline.push(item);
    true
}

/// Cut the item under `frame` into two at that point, changing nothing about
/// what the timeline plays — only where its boundaries are.
///
/// A no-op when the playhead is already on a cut, or past the end.
pub fn split_at(project: &mut Project, frame: i64) -> bool {
    let Some((index, offset)) = item_at(&project.timeline, frame) else {
        return false;
    };
    if offset == 0 {
        return false;
    }
    let existing = project.timeline[index];
    project.timeline[index] = TimelineItem {
        clip_id: existing.clip_id,
        in_frame: existing.in_frame,
        out_frame: existing.in_frame + offset - 1,
    };
    project.timeline.insert(
        index + 1,
        TimelineItem {
            clip_id: existing.clip_id,
            in_frame: existing.in_frame + offset,
            out_frame: existing.out_frame,
        },
    );
    true
}

/// Insert a marked source range at `playhead`, rippling everything after it.
/// Splits the item under the playhead when the playhead is mid-clip.
///
/// Returns the frame at which the inserted material starts, or `None` if
/// nothing was inserted.
pub fn insert_at(
    project: &mut Project,
    playhead: i64,
    clip_id: ClipId,
    in_frame: i64,
    out_frame: i64,
) -> Option<i64> {
    let item = make_item(project, clip_id, in_frame, out_frame)?;
    let playhead = playhead.max(0);
    let total = total_frames(&project.timeline);

    if playhead >= total {
        project.timeline.push(item);
        return Some(total);
    }

    // Make sure there is a boundary here, then insert at it. Splitting lives
    // in one place so insert and the `S` key cannot disagree about it.
    split_at(project, playhead);
    let index = item_at(&project.timeline, playhead)
        .map(|(i, _)| i)
        .unwrap_or(project.timeline.len());
    project.timeline.insert(index, item);
    Some(playhead)
}

/// Remove an item and close the gap.
pub fn ripple_delete(project: &mut Project, index: usize) -> bool {
    if index >= project.timeline.len() {
        return false;
    }
    project.timeline.remove(index);
    true
}

/// Trim the head of `index` so its first frame lands at `playhead`.
/// The clip shortens and everything after it ripples earlier.
pub fn trim_head(project: &mut Project, index: usize, playhead: i64) -> bool {
    let Some(item) = project.timeline.get(index).copied() else {
        return false;
    };
    let start = item_start(&project.timeline, index);
    let offset = playhead - start;
    // Must stay inside the clip and leave at least one frame.
    if offset <= 0 || offset >= item.len() {
        return false;
    }
    project.timeline[index].in_frame = item.in_frame + offset;
    true
}

/// Trim the tail of `index` so its last frame is the one before `playhead`.
/// The clip shortens and everything after it ripples earlier.
pub fn trim_tail(project: &mut Project, index: usize, playhead: i64) -> bool {
    let Some(item) = project.timeline.get(index).copied() else {
        return false;
    };
    let start = item_start(&project.timeline, index);
    let offset = playhead - start;
    // `offset` frames survive; need at least one, and fewer than we have.
    if offset <= 0 || offset >= item.len() {
        return false;
    }
    project.timeline[index].out_frame = item.in_frame + offset - 1;
    true
}

/// Move the item at `from` so it sits at index `to`, sliding the rest along.
/// Returns false when either index is out of range or nothing would change.
pub fn reorder(project: &mut Project, from: usize, to: usize) -> bool {
    let len = project.timeline.len();
    if from >= len || to >= len || from == to {
        return false;
    }
    let item = project.timeline.remove(from);
    project.timeline.insert(to, item);
    true
}

/// Move an item one position earlier (`delta == -1`) or later (`delta == 1`).
/// Returns the item's new index.
pub fn move_item(project: &mut Project, index: usize, delta: isize) -> Option<usize> {
    let target = index as isize + delta;
    if target < 0 {
        return None;
    }
    let target = target as usize;
    reorder(project, index, target).then_some(target)
}

/// Clamp a source range to the clip's real extent and reject empty ranges.
fn make_item(
    project: &Project,
    clip_id: ClipId,
    in_frame: i64,
    out_frame: i64,
) -> Option<TimelineItem> {
    let clip = project.clip(clip_id)?;
    if clip.duration_frames <= 0 {
        return None;
    }
    let last = clip.last_frame();
    let i = in_frame.clamp(0, last);
    let o = out_frame.clamp(0, last);
    if o < i {
        return None;
    }
    Some(TimelineItem {
        clip_id,
        in_frame: i,
        out_frame: o,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SourceClip;
    use std::path::PathBuf;

    fn clip(project: &mut Project, frames: i64) -> ClipId {
        let id = ClipId::new();
        project.clips.push(SourceClip {
            id,
            path: PathBuf::from(format!("/tmp/{id}.mp4")),
            proxy_path: None,
            duration_frames: frames,
            native_frames: frames,
            native_fps_num: 30000,
            native_fps_den: 1001,
            width: 1920,
            height: 1080,
            sample_aspect_num: 1,
            sample_aspect_den: 1,
            progressive: true,
            colorspace: 709,
            has_audio: true,
            video_index: 0,
            audio_index: 1,
            mark_in: None,
            mark_out: None,
            rate_mismatch: false,
            variable_rate: false,
        });
        id
    }

    fn project_with(lengths: &[(i64, i64)]) -> (Project, ClipId) {
        let mut p = Project::new();
        let id = clip(&mut p, 1000);
        for &(i, o) in lengths {
            append(&mut p, id, i, o);
        }
        (p, id)
    }

    #[test]
    fn append_uses_inclusive_lengths() {
        let (p, _) = project_with(&[(0, 99), (100, 199)]);
        assert_eq!(p.timeline.len(), 2);
        assert_eq!(p.timeline[0].len(), 100);
        assert_eq!(total_frames(&p.timeline), 200);
    }

    #[test]
    fn append_rejects_inverted_range() {
        let mut p = Project::new();
        let id = clip(&mut p, 100);
        assert!(!append(&mut p, id, 50, 20));
        assert!(p.timeline.is_empty());
    }

    #[test]
    fn append_clamps_to_clip_extent() {
        let mut p = Project::new();
        let id = clip(&mut p, 100);
        assert!(append(&mut p, id, 0, 500));
        assert_eq!(p.timeline[0].out_frame, 99);
    }

    #[test]
    fn item_at_maps_frames_to_items() {
        let (p, _) = project_with(&[(0, 99), (200, 299)]);
        assert_eq!(item_at(&p.timeline, 0), Some((0, 0)));
        assert_eq!(item_at(&p.timeline, 99), Some((0, 99)));
        assert_eq!(item_at(&p.timeline, 100), Some((1, 0)));
        assert_eq!(item_at(&p.timeline, 199), Some((1, 99)));
        assert_eq!(item_at(&p.timeline, 200), None);
    }

    #[test]
    fn cut_points_include_start_and_end() {
        let (p, _) = project_with(&[(0, 99), (0, 49)]);
        assert_eq!(cut_points(&p.timeline), vec![0, 100, 150]);
        assert_eq!(prev_cut(&p.timeline, 120), Some(100));
        assert_eq!(prev_cut(&p.timeline, 0), None);
        assert_eq!(next_cut(&p.timeline, 100), Some(150));
        assert_eq!(next_cut(&p.timeline, 150), None);
    }

    #[test]
    fn insert_mid_clip_splits_and_ripples() {
        let (mut p, id) = project_with(&[(0, 99)]);
        // Insert 10 frames at frame 40.
        assert_eq!(insert_at(&mut p, 40, id, 500, 509), Some(40));
        assert_eq!(p.timeline.len(), 3);
        assert_eq!((p.timeline[0].in_frame, p.timeline[0].out_frame), (0, 39));
        assert_eq!((p.timeline[1].in_frame, p.timeline[1].out_frame), (500, 509));
        assert_eq!((p.timeline[2].in_frame, p.timeline[2].out_frame), (40, 99));
        // No frames lost, ten gained.
        assert_eq!(total_frames(&p.timeline), 110);
    }

    #[test]
    fn split_divides_a_clip_without_changing_what_plays() {
        let (mut p, _) = project_with(&[(100, 199)]);
        assert!(split_at(&mut p, 40));
        assert_eq!(p.timeline.len(), 2);
        assert_eq!((p.timeline[0].in_frame, p.timeline[0].out_frame), (100, 139));
        assert_eq!((p.timeline[1].in_frame, p.timeline[1].out_frame), (140, 199));
        // The defining property: nothing gained, nothing lost.
        assert_eq!(total_frames(&p.timeline), 100);
        assert_eq!(cut_points(&p.timeline), vec![0, 40, 100]);
    }

    #[test]
    fn split_on_an_existing_cut_is_a_no_op() {
        let (mut p, _) = project_with(&[(0, 49), (0, 49)]);
        assert!(!split_at(&mut p, 0));
        assert!(!split_at(&mut p, 50), "50 is already a boundary");
        assert!(!split_at(&mut p, 100), "past the end");
        assert_eq!(p.timeline.len(), 2);
    }

    #[test]
    fn split_is_repeatable() {
        let (mut p, _) = project_with(&[(0, 99)]);
        assert!(split_at(&mut p, 50));
        assert!(split_at(&mut p, 25));
        assert!(split_at(&mut p, 75));
        assert_eq!(p.timeline.len(), 4);
        assert_eq!(total_frames(&p.timeline), 100);
        assert_eq!(cut_points(&p.timeline), vec![0, 25, 50, 75, 100]);
    }

    #[test]
    fn insert_on_a_cut_does_not_split() {
        let (mut p, id) = project_with(&[(0, 99), (0, 99)]);
        assert_eq!(insert_at(&mut p, 100, id, 0, 9), Some(100));
        assert_eq!(p.timeline.len(), 3);
        assert_eq!(p.timeline[1].len(), 10);
        assert_eq!(total_frames(&p.timeline), 210);
    }

    #[test]
    fn insert_past_end_appends() {
        let (mut p, id) = project_with(&[(0, 99)]);
        assert_eq!(insert_at(&mut p, 999, id, 0, 9), Some(100));
        assert_eq!(p.timeline.len(), 2);
    }

    #[test]
    fn ripple_delete_closes_the_gap() {
        let (mut p, _) = project_with(&[(0, 99), (0, 49), (0, 9)]);
        assert!(ripple_delete(&mut p, 1));
        assert_eq!(total_frames(&p.timeline), 110);
        assert_eq!(item_start(&p.timeline, 1), 100);
        assert!(!ripple_delete(&mut p, 5));
    }

    #[test]
    fn trim_head_shortens_from_the_front() {
        let (mut p, _) = project_with(&[(0, 99), (0, 99)]);
        // Playhead at 30, inside item 0.
        assert!(trim_head(&mut p, 0, 30));
        assert_eq!((p.timeline[0].in_frame, p.timeline[0].out_frame), (30, 99));
        assert_eq!(total_frames(&p.timeline), 170);
        // Second item now starts at 70.
        assert_eq!(item_start(&p.timeline, 1), 70);
    }

    #[test]
    fn trim_tail_shortens_from_the_back() {
        let (mut p, _) = project_with(&[(0, 99), (0, 99)]);
        assert!(trim_tail(&mut p, 0, 30));
        assert_eq!((p.timeline[0].in_frame, p.timeline[0].out_frame), (0, 29));
        assert_eq!(p.timeline[0].len(), 30);
        assert_eq!(total_frames(&p.timeline), 130);
    }

    #[test]
    fn trims_refuse_to_empty_a_clip() {
        let (mut p, _) = project_with(&[(0, 99)]);
        assert!(!trim_head(&mut p, 0, 0), "trimming to the clip start is a no-op");
        assert!(!trim_tail(&mut p, 0, 0), "trimming away every frame is refused");
        assert!(!trim_head(&mut p, 0, 100), "playhead past the clip is refused");
        assert_eq!(p.timeline[0].len(), 100);
    }

    #[test]
    fn reorder_moves_an_item_anywhere() {
        let (mut p, _) = project_with(&[(0, 9), (0, 19), (0, 29)]);
        // Drag the last one to the front.
        assert!(reorder(&mut p, 2, 0));
        assert_eq!(
            p.timeline.iter().map(|i| i.len()).collect::<Vec<_>>(),
            vec![30, 10, 20]
        );
        // Total length never changes when only the order does.
        assert_eq!(total_frames(&p.timeline), 60);

        // And back again.
        assert!(reorder(&mut p, 0, 2));
        assert_eq!(
            p.timeline.iter().map(|i| i.len()).collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
    }

    #[test]
    fn reorder_rejects_nonsense() {
        let (mut p, _) = project_with(&[(0, 9), (0, 19)]);
        assert!(!reorder(&mut p, 0, 0), "moving onto itself changes nothing");
        assert!(!reorder(&mut p, 5, 0));
        assert!(!reorder(&mut p, 0, 5));
        assert_eq!(p.timeline.len(), 2);
    }

    #[test]
    fn move_item_reorders() {
        let (mut p, _) = project_with(&[(0, 9), (0, 19), (0, 29)]);
        assert_eq!(move_item(&mut p, 2, -1), Some(1));
        assert_eq!(p.timeline[1].len(), 30);
        assert_eq!(move_item(&mut p, 0, -1), None);
        assert_eq!(move_item(&mut p, 2, 1), None);
    }

    #[test]
    fn removing_a_bin_clip_refuses_while_the_timeline_uses_it() {
        let (mut p, id) = project_with(&[(0, 9), (0, 19)]);
        assert_eq!(p.timeline_uses(id), 2);
        assert!(!p.remove_clip(id), "must refuse while cuts reference it");
        assert_eq!(p.clips.len(), 1);

        // Once the cuts are gone it can go.
        p.timeline.clear();
        assert_eq!(p.timeline_uses(id), 0);
        assert!(p.remove_clip(id));
        assert!(p.clips.is_empty());
        // And removing it twice is not an error worth reporting differently.
        assert!(!p.remove_clip(id));
    }

    #[test]
    fn last_frame_of_empty_timeline_is_zero() {
        let p = Project::new();
        assert_eq!(last_frame(&p.timeline), 0);
        assert_eq!(total_frames(&p.timeline), 0);
    }
}
