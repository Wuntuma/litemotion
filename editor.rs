//! Editing session: current project + undo/redo history + selection.
use crate::error::Result;
use crate::project::model::{Clip, Project};

const MAX_HISTORY: usize = 200;

pub struct Editor {
    pub project: Project,
    undo: Vec<(String, Project)>,
    redo: Vec<(String, Project)>,
    pub selection: Vec<u64>,
    pub clipboard: Vec<(u64, Clip)>,
    /// Unsaved changes since last save/load.
    pub dirty: bool,
    /// Bumped on every change; autosave/preview use it to detect staleness.
    pub revision: u64,
    last_coalesce: Option<String>,
}

impl Editor {
    pub fn new(project: Project) -> Editor {
        Editor { project, undo: vec![], redo: vec![], selection: vec![], clipboard: vec![], dirty: false, revision: 0, last_coalesce: None }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|(l, _)| l.as_str())
    }

    fn push_undo(&mut self, label: &str) {
        self.undo.push((label.to_string(), self.project.clone()));
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Run an edit. On error the project is restored and nothing is recorded.
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut Project) -> Result<R>) -> Result<R> {
        self.last_coalesce = None;
        let before = self.project.clone();
        match f(&mut self.project) {
            Ok(r) => {
                if self.project != before {
                    self.undo.push((label.to_string(), before));
                    if self.undo.len() > MAX_HISTORY {
                        self.undo.remove(0);
                    }
                    self.redo.clear();
                    self.dirty = true;
                    self.revision += 1;
                }
                Ok(r)
            }
            Err(e) => {
                self.project = before;
                Err(e)
            }
        }
    }

    /// Like `edit`, but consecutive calls with the same `key` (a slider being
    /// dragged) share ONE undo step.
    pub fn edit_coalesced(&mut self, key: &str, f: impl FnOnce(&mut Project) -> Result<()>) -> Result<()> {
        let same = self.last_coalesce.as_deref() == Some(key);
        if !same {
            self.push_undo(key);
        }
        let before = self.project.clone();
        match f(&mut self.project) {
            Ok(()) => {
                self.last_coalesce = Some(key.to_string());
                self.dirty = true;
                self.revision += 1;
                Ok(())
            }
            Err(e) => {
                self.project = before;
                if !same {
                    self.undo.pop();
                }
                Err(e)
            }
        }
    }
    pub fn end_coalesce(&mut self) {
        self.last_coalesce = None;
    }

    pub fn undo(&mut self) -> bool {
        self.last_coalesce = None;
        let Some((label, prev)) = self.undo.pop() else { return false };
        let cur = std::mem::replace(&mut self.project, prev);
        self.redo.push((label, cur));
        self.after_history_change();
        true
    }
    pub fn redo(&mut self) -> bool {
        self.last_coalesce = None;
        let Some((label, next)) = self.redo.pop() else { return false };
        let cur = std::mem::replace(&mut self.project, next);
        self.undo.push((label, cur));
        self.after_history_change();
        true
    }
    fn after_history_change(&mut self) {
        self.dirty = true;
        self.revision += 1;
        let p = &self.project;
        self.selection.retain(|id| p.clip(*id).is_some());
    }
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub fn copy_selection(&mut self) {
        self.clipboard = self
            .selection
            .iter()
            .filter_map(|id| {
                let (t, c) = self.project.find_clip(*id)?;
                Some((self.project.tracks[t].id, self.project.tracks[t].clips[c].clone()))
            })
            .collect();
    }
}
