//! What a drag inside the window carries when it isn't a tab: files picked up from the explorer.
//! The explorer starts the drag; the explorer's folder rows, every pane's tab bar and every
//! pane's edges take the drop (a tab bar opens the files there, an edge opens them in a split).
//! Files from Finder arrive as GPUI's `ExternalPaths` instead.

use std::{path::PathBuf, rc::Rc};

/// Files or folders dragged from the explorer, in row order. Shared, so every picked row's
/// drag holds the one list.
#[derive(Clone, Debug)]
pub struct DraggedFiles(pub Rc<[PathBuf]>);

/// What a pane takes from a drop: a tab from a tab bar, or files (from the explorer or Finder).
#[derive(Clone, Debug)]
pub enum Dropped {
    Tab(gpui_kit::EntityId),
    Files(Vec<PathBuf>),
}

/// Where on a pane the drop landed: a bar's tab slot, the middle of the editor, or an edge band
/// (where a new pane opens).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Bar(usize),
    Middle,
    Left,
    Right,
    Top,
    Bottom,
}
