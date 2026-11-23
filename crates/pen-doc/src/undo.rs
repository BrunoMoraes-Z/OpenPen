//! Undo/redo in blocks: every op applied inside `begin()`/`commit()` becomes
//! one undo step (blocks nest; only the outermost commit closes the step).

use crate::ops::{DocOp, OpError};
use crate::scene::SceneDoc;

#[derive(Debug, Default)]
pub struct UndoStack {
    undo: Vec<Vec<DocOp>>,
    redo: Vec<Vec<DocOp>>,
    /// Inverses collected by the open block, in application order.
    open: Vec<DocOp>,
    depth: u32,
    /// Max number of undo steps kept (0 = unlimited).
    pub limit: usize,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a block (nestable).
    pub fn begin(&mut self) {
        self.depth += 1;
    }

    /// Closes a block; the outermost commit records one undo step.
    pub fn commit(&mut self) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth == 0 && !self.open.is_empty() {
            let block = std::mem::take(&mut self.open);
            self.push(block);
        }
    }

    /// Reverts the ops of the open block and closes it without recording.
    pub fn cancel(&mut self, scene: &mut SceneDoc) -> Result<(), OpError> {
        self.depth = 0;
        let block = std::mem::take(&mut self.open);
        for op in block.into_iter().rev() {
            scene.apply(op)?;
        }
        Ok(())
    }

    pub fn in_block(&self) -> bool {
        self.depth > 0
    }

    fn push(&mut self, block: Vec<DocOp>) {
        self.undo.push(block);
        self.redo.clear();
        if self.limit > 0 && self.undo.len() > self.limit {
            let excess = self.undo.len() - self.limit;
            self.undo.drain(..excess);
        }
    }

    /// Applies `op` and records its inverse (a single-op step when no block is open).
    pub fn apply(&mut self, scene: &mut SceneDoc, op: DocOp) -> Result<(), OpError> {
        let inv = scene.apply(op)?;
        if self.depth == 0 {
            self.push(vec![inv]);
        } else {
            self.open.push(inv);
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Undoes the last step; returns false when there is nothing to undo.
    pub fn undo(&mut self, scene: &mut SceneDoc) -> Result<bool, OpError> {
        let Some(block) = self.undo.pop() else {
            return Ok(false);
        };
        let redo = replay(scene, block)?;
        self.redo.push(redo);
        Ok(true)
    }

    pub fn redo(&mut self, scene: &mut SceneDoc) -> Result<bool, OpError> {
        let Some(block) = self.redo.pop() else {
            return Ok(false);
        };
        let undo = replay(scene, block)?;
        self.undo.push(undo);
        Ok(true)
    }
}

/// Applies the inverses of a block in reverse order; returns the block that
/// reverts this replay.
fn replay(scene: &mut SceneDoc, block: Vec<DocOp>) -> Result<Vec<DocOp>, OpError> {
    let mut out = Vec::with_capacity(block.len());
    for op in block.into_iter().rev() {
        out.push(scene.apply(op)?);
    }
    Ok(out)
}
