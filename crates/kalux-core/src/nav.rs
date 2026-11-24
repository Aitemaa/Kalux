//! Folder playlist and natural sort.

use std::cmp::Ordering;
use std::io;
use std::path::{Path, PathBuf};

/// Natural, case-insensitive ordering: `img2.jpg` < `img10.jpg`, `A.png` == `a.png` (ties broken
/// by the raw string so the order is total and stable).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let _ = (a, b);
    todo!("kalux-core")
}

/// The ordered list of images in the folder of the opened file, plus the current position.
#[derive(Clone, Debug)]
pub struct Playlist {
    files: Vec<PathBuf>,
    index: usize,
}

impl Playlist {
    /// A playlist containing only `file` (used for the first frame before the folder scan finishes).
    pub fn single(file: PathBuf) -> Self {
        Self {
            files: vec![file],
            index: 0,
        }
    }

    /// Build from an explicit list (already filtered). Sorts with [`natural_cmp`] on file names
    /// and positions on `current` (falls back to index 0 if absent).
    pub fn from_files(files: Vec<PathBuf>, current: &Path) -> Self {
        let _ = (files, current);
        todo!("kalux-core")
    }

    /// List `file`'s parent directory, keep entries whose extension is in
    /// [`crate::SUPPORTED_EXTENSIONS`] (case-insensitive, regular files only, hidden/system files
    /// skipped where detectable), sort naturally, and position on `file`.
    /// `file` is always included even if its extension is unknown.
    pub fn scan_folder_of(file: &Path) -> io::Result<Self> {
        let _ = file;
        todo!("kalux-core")
    }

    pub fn current(&self) -> &Path {
        &self.files[self.index]
    }
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn len(&self) -> usize {
        self.files.len()
    }
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Move by `delta` (wraps around both ends). Returns the new current path.
    pub fn step(&mut self, delta: isize) -> &Path {
        let _ = delta;
        todo!("kalux-core")
    }
    pub fn go_first(&mut self) -> &Path {
        todo!("kalux-core")
    }
    pub fn go_last(&mut self) -> &Path {
        todo!("kalux-core")
    }

    /// Paths to preload around the current one, nearest first, alternating forward/backward:
    /// `+1, -1, +2, -2, …` up to `radius`, wrapping, without duplicates and excluding current.
    pub fn neighbors(&self, radius: usize) -> Vec<PathBuf> {
        let _ = radius;
        todo!("kalux-core")
    }

    /// Remove the current entry (after delete-to-recycle-bin). Current becomes the next file
    /// (or the previous if it was last). Returns `None` if the playlist is now empty.
    pub fn remove_current(&mut self) -> Option<&Path> {
        todo!("kalux-core")
    }
}
