//! Deliberately naive undo: a snapshot of the whole `Project` per mutation.
//!
//! The model is a few kilobytes even with hundreds of clips, so full snapshots
//! cost nothing and are impossible to get wrong. There is no command pattern
//! here and there should never be one.

use crate::model::Project;

pub const HISTORY_CAP: usize = 500;

#[derive(Debug, Default)]
pub struct History {
    past: Vec<Project>,
    future: Vec<Project>,
    cap: usize,
}

impl History {
    pub fn new() -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
            cap: HISTORY_CAP,
        }
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// Record the state *before* a mutation. Call this immediately before
    /// changing the project. Redo history is discarded, as it must be.
    pub fn snapshot(&mut self, before: &Project) {
        self.past.push(before.clone());
        if self.past.len() > self.cap {
            let excess = self.past.len() - self.cap;
            self.past.drain(0..excess);
        }
        self.future.clear();
    }

    /// Forget everything — used when a project is opened or created.
    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn depth(&self) -> usize {
        self.past.len()
    }

    /// Replace `current` with the previous state, banking `current` for redo.
    pub fn undo(&mut self, current: &mut Project) -> bool {
        let Some(prev) = self.past.pop() else {
            return false;
        };
        let now = std::mem::replace(current, prev);
        self.future.push(now);
        true
    }

    /// Replace `current` with the next state, banking `current` for undo.
    pub fn redo(&mut self, current: &mut Project) -> bool {
        let Some(next) = self.future.pop() else {
            return false;
        };
        let now = std::mem::replace(current, next);
        self.past.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Profile;

    fn with_width(w: u32) -> Project {
        Project {
            profile: Profile {
                width: w,
                ..Profile::default()
            },
            ..Project::new()
        }
    }

    #[test]
    fn undo_and_redo_round_trip() {
        let mut h = History::new();
        let mut p = with_width(1);
        h.snapshot(&p);
        p = with_width(2);
        h.snapshot(&p);
        p = with_width(3);

        assert!(h.undo(&mut p));
        assert_eq!(p.profile.width, 2);
        assert!(h.undo(&mut p));
        assert_eq!(p.profile.width, 1);
        assert!(!h.undo(&mut p), "history exhausted");

        assert!(h.redo(&mut p));
        assert_eq!(p.profile.width, 2);
        assert!(h.redo(&mut p));
        assert_eq!(p.profile.width, 3);
        assert!(!h.redo(&mut p));
    }

    #[test]
    fn a_new_mutation_discards_redo() {
        let mut h = History::new();
        let mut p = with_width(1);
        h.snapshot(&p);
        p = with_width(2);
        h.undo(&mut p);
        assert!(h.can_redo());
        h.snapshot(&p);
        assert!(!h.can_redo(), "redo must be dropped after a fresh edit");
    }

    #[test]
    fn history_is_capped_and_drops_the_oldest() {
        let mut h = History::with_cap(3);
        for w in 1..=10u32 {
            h.snapshot(&with_width(w));
        }
        assert_eq!(h.depth(), 3);
        let mut p = with_width(99);
        // The three retained snapshots are widths 8, 9, 10.
        h.undo(&mut p);
        assert_eq!(p.profile.width, 10);
        h.undo(&mut p);
        assert_eq!(p.profile.width, 9);
        h.undo(&mut p);
        assert_eq!(p.profile.width, 8);
        assert!(!h.can_undo());
    }
}
