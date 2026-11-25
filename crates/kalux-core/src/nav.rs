//! Folder playlist and natural sort.

use std::cmp::Ordering;
use std::io;
use std::path::{Path, PathBuf};

use crate::ImageFormat;

/// Streaming tokenizer for natural case-insensitive comparison.
///
/// Every non-digit character is expanded via `char::to_lowercase()`, while each maximal
/// contiguous ASCII digit run (`'0'..='9'`) becomes a single `Token::Digits` stripped of
/// leading zeros. Because ASCII digits sit strictly between characters `< '0'` and `> '9'`,
/// ordering `Token` by `(rank, value)` induces a pure key function `key(s)` whose
/// lexicographical order (broken by `a.cmp(b)`) is a strict total order.
#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    CharBeforeDigits(char),
    /// Significant digits (leading `'0'` stripped; `""` represents zero).
    Digits(&'a str),
    CharAfterDigits(char),
}

impl Ord for Token<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        use Token::*;
        match (self, other) {
            (CharBeforeDigits(a), CharBeforeDigits(b)) => a.cmp(b),
            (CharBeforeDigits(_), _) => Ordering::Less,
            (_, CharBeforeDigits(_)) => Ordering::Greater,
            (Digits(a), Digits(b)) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
            (Digits(_), CharAfterDigits(_)) => Ordering::Less,
            (CharAfterDigits(_), Digits(_)) => Ordering::Greater,
            (CharAfterDigits(a), CharAfterDigits(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Token<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct NaturalTokens<'a> {
    rest: &'a str,
    pending_lower: Option<std::char::ToLowercase>,
}

impl<'a> NaturalTokens<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            rest: s,
            pending_lower: None,
        }
    }
}

impl<'a> Iterator for NaturalTokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(ref mut lower) = self.pending_lower {
            if let Some(c) = lower.next() {
                return Some(if c < '0' {
                    Token::CharBeforeDigits(c)
                } else {
                    Token::CharAfterDigits(c)
                });
            }
            self.pending_lower = None;
        }

        let first = *self.rest.as_bytes().first()?;
        if first.is_ascii_digit() {
            let end = self
                .rest
                .as_bytes()
                .iter()
                .position(|b| !b.is_ascii_digit())
                .unwrap_or(self.rest.len());
            let run = &self.rest[..end];
            self.rest = &self.rest[end..];
            let sig = run.trim_start_matches('0');
            return Some(Token::Digits(sig));
        }

        let mut chars = self.rest.chars();
        let ch = chars.next()?;
        self.rest = chars.as_str();
        if ch.is_ascii() {
            let lc = ch.to_ascii_lowercase();
            Some(if lc < '0' {
                Token::CharBeforeDigits(lc)
            } else {
                Token::CharAfterDigits(lc)
            })
        } else {
            let mut lower = ch.to_lowercase();
            let lc = lower.next()?;
            self.pending_lower = Some(lower);
            Some(if lc < '0' {
                Token::CharBeforeDigits(lc)
            } else {
                Token::CharAfterDigits(lc)
            })
        }
    }
}

/// Natural, case-insensitive ordering: `img2.jpg` < `img10.jpg`, `A.png` == `a.png` (ties broken
/// by the raw string so the order is total and stable).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ta = NaturalTokens::new(a);
    let mut tb = NaturalTokens::new(b);
    loop {
        match (ta.next(), tb.next()) {
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                ord => return ord,
            },
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return a.cmp(b),
        }
    }
}

/// The ordered list of images in the folder of the opened file, plus the current position.
#[derive(Clone, Debug)]
pub struct Playlist {
    files: Vec<PathBuf>,
    index: usize,
}

fn path_sort_cmp(a: &Path, b: &Path) -> Ordering {
    let na = a
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_else(|| a.to_string_lossy());
    let nb = b
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_else(|| b.to_string_lossy());
    natural_cmp(&na, &nb).then_with(|| a.as_os_str().cmp(b.as_os_str()))
}

fn same_file_name(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    #[cfg(windows)]
    {
        if a == b {
            return true;
        }
        match (a.to_str(), b.to_str()) {
            (Some(sa), Some(sb)) => sa.eq_ignore_ascii_case(sb),
            _ => false,
        }
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn paths_match(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a == b
            || a.to_str()
                .zip(b.to_str())
                .is_some_and(|(sa, sb)| sa.eq_ignore_ascii_case(sb))
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn is_hidden_entry(entry: &std::fs::DirEntry) -> bool {
    let dot_file = entry.file_name().to_string_lossy().starts_with('.');
    #[cfg(windows)]
    {
        if dot_file {
            return true;
        }
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
        entry.metadata().is_ok_and(|meta| {
            (meta.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM)) != 0
        })
    }
    #[cfg(not(windows))]
    {
        dot_file
    }
}

fn is_regular_file(entry: &std::fs::DirEntry) -> bool {
    let Ok(ft) = entry.file_type() else {
        return false;
    };
    if ft.is_file() {
        return true;
    }
    if ft.is_symlink() {
        return std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file());
    }
    false
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
    pub fn from_files(mut files: Vec<PathBuf>, current: &Path) -> Self {
        files.sort_by(|a, b| path_sort_cmp(a, b));
        let index = files
            .iter()
            .position(|p| paths_match(p, current))
            .unwrap_or(0);
        Self { files, index }
    }

    /// List `file`'s parent directory, keep entries whose extension is in
    /// [`crate::SUPPORTED_EXTENSIONS`] (case-insensitive, regular files only, hidden/system files
    /// skipped where detectable), sort naturally, and position on `file`.
    /// `file` is always included even if its extension is unknown.
    pub fn scan_folder_of(file: &Path) -> io::Result<Self> {
        let dir = match file.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let target_name = file.file_name();
        let mut files = Vec::new();
        let mut found_current = false;

        for entry in std::fs::read_dir(dir)? {
            let Ok(entry) = entry else { continue };
            let entry_path = entry.path();
            let is_current = entry_path == file
                || target_name.is_some_and(|tn| same_file_name(&entry.file_name(), tn));
            if is_current {
                if !found_current {
                    files.push(file.to_path_buf());
                    found_current = true;
                }
                continue;
            }
            if is_hidden_entry(&entry) {
                continue;
            }
            if !is_regular_file(&entry) {
                continue;
            }
            if ImageFormat::from_path(&entry_path).is_some() {
                files.push(entry_path);
            }
        }

        if !found_current {
            files.push(file.to_path_buf());
        }

        Ok(Self::from_files(files, file))
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
        let len = self.files.len() as i128;
        let next = (self.index as i128 + delta as i128).rem_euclid(len) as usize;
        self.index = next;
        &self.files[self.index]
    }
    pub fn go_first(&mut self) -> &Path {
        self.index = 0;
        &self.files[0]
    }
    pub fn go_last(&mut self) -> &Path {
        self.index = self.files.len() - 1;
        &self.files[self.index]
    }

    /// Paths to preload around the current one, nearest first, alternating forward/backward:
    /// `+1, -1, +2, -2, …` up to `radius`, wrapping, without duplicates and excluding current.
    pub fn neighbors(&self, radius: usize) -> Vec<PathBuf> {
        let len = self.files.len();
        if len <= 1 || radius == 0 {
            return Vec::new();
        }
        let max_out = len - 1;
        let max_d = radius.min(len);
        let mut out = Vec::with_capacity((radius.saturating_mul(2)).min(max_out));
        let mut seen = Vec::with_capacity(out.capacity() + 1);
        seen.push(self.index);

        for d in 1..=max_d {
            let offset = d % len;
            let fwd = (self.index + offset) % len;
            if !seen.contains(&fwd) {
                seen.push(fwd);
                out.push(self.files[fwd].clone());
                if out.len() == max_out {
                    break;
                }
            }
            let bwd = (self.index + len - offset) % len;
            if !seen.contains(&bwd) {
                seen.push(bwd);
                out.push(self.files[bwd].clone());
                if out.len() == max_out {
                    break;
                }
            }
        }
        out
    }

    /// Remove the current entry (after delete-to-recycle-bin). Current becomes the next file
    /// (or the previous if it was last). Returns `None` if the playlist is now empty.
    pub fn remove_current(&mut self) -> Option<&Path> {
        if self.files.is_empty() {
            return None;
        }
        self.files.remove(self.index);
        if self.files.is_empty() {
            self.index = 0;
            return None;
        }
        if self.index >= self.files.len() {
            self.index = self.files.len() - 1;
        }
        Some(&self.files[self.index])
    }

    /// Insert `path` in natural sort order (if not already present) and select it as current.
    pub fn insert_and_select(&mut self, path: PathBuf) -> &Path {
        if let Some(pos) = self.files.iter().position(|p| paths_match(p, &path)) {
            self.index = pos;
            return &self.files[self.index];
        }
        let pos = self
            .files
            .binary_search_by(|existing| path_sort_cmp(existing, &path))
            .unwrap_or_else(|i| i);
        self.files.insert(pos, path);
        self.index = pos;
        &self.files[self.index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_cmp_basic_and_case() {
        assert_eq!(natural_cmp("img2.jpg", "img10.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("img10.jpg", "img2.jpg"), Ordering::Greater);
        assert_eq!(natural_cmp("a.png", "b.png"), Ordering::Less);
        assert_eq!(natural_cmp("A.png", "b.png"), Ordering::Less);
        assert_eq!(natural_cmp("a.png", "B.png"), Ordering::Less);
        // Raw string tiebreak: 'A' (65) < 'a' (97)
        assert_eq!(natural_cmp("A.png", "a.png"), Ordering::Less);
        assert_eq!(natural_cmp("a.png", "a.png"), Ordering::Equal);
    }

    #[test]
    fn natural_cmp_leading_zeros_and_huge_numbers() {
        assert_eq!(natural_cmp("img01.jpg", "img2.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("img002.jpg", "img10.jpg"), Ordering::Less);
        // Equal numeric value: tiebreak by raw string ('0' < '2')
        assert_eq!(natural_cmp("img02.jpg", "img2.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("0", "00"), Ordering::Less);

        // Huge numbers well beyond u128::MAX (39+ digits)
        let big1 = "pic_99999999999999999999999999999999999999999999999999.jpg";
        let big2 = "pic_100000000000000000000000000000000000000000000000000.jpg";
        assert_eq!(natural_cmp(big1, big2), Ordering::Less);
        assert_eq!(natural_cmp(big2, big1), Ordering::Greater);
    }

    #[test]
    fn natural_cmp_is_strict_total_order() {
        let samples = [
            "",
            "a",
            "A",
            "a0",
            "a00",
            "a1",
            "a01",
            "a2",
            "a10",
            "a-1",
            "a_1",
            "A1",
            "a1b",
            "a01a",
            "a01b",
            "0",
            "00",
            "1",
            "2",
            "10",
            "img2.jpg",
            "img10.jpg",
            "IMG2.JPG",
            "img02.jpg",
        ];
        // Verify reflexivity, antisymmetry, and transitivity on all triples.
        for &x in &samples {
            assert_eq!(natural_cmp(x, x), Ordering::Equal);
            for &y in &samples {
                let xy = natural_cmp(x, y);
                let yx = natural_cmp(y, x);
                assert_eq!(xy, yx.reverse(), "antisymmetry failed for {x:?} vs {y:?}");
                if x != y {
                    assert_ne!(xy, Ordering::Equal, "total order failed for {x:?} vs {y:?}");
                }
                for &z in &samples {
                    if xy == Ordering::Less && natural_cmp(y, z) == Ordering::Less {
                        assert_eq!(
                            natural_cmp(x, z),
                            Ordering::Less,
                            "transitivity failed for {x:?} < {y:?} < {z:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn playlist_from_files_and_navigation() {
        let files = vec![
            PathBuf::from("img10.jpg"),
            PathBuf::from("img2.jpg"),
            PathBuf::from("img1.jpg"),
        ];
        let mut pl = Playlist::from_files(files, Path::new("img2.jpg"));
        assert_eq!(pl.len(), 3);
        assert_eq!(pl.index(), 1);
        assert_eq!(pl.current(), Path::new("img2.jpg"));

        assert_eq!(pl.step(1), Path::new("img10.jpg"));
        assert_eq!(pl.step(1), Path::new("img1.jpg")); // wrap forward
        assert_eq!(pl.step(-1), Path::new("img10.jpg")); // wrap backward
        assert_eq!(pl.step(-5), Path::new("img1.jpg")); // multi-wrap negative
        assert_eq!(pl.go_last(), Path::new("img10.jpg"));
        assert_eq!(pl.go_first(), Path::new("img1.jpg"));
    }

    #[test]
    fn playlist_neighbors_order_wrap_and_tiny() {
        // 5 items, index 1 -> +1 (2), -1 (0), +2 (3), -2 (4)
        let files: Vec<PathBuf> = (0..5).map(|i| PathBuf::from(format!("f{i}.png"))).collect();
        let pl = Playlist::from_files(files, Path::new("f1.png"));
        assert_eq!(
            pl.neighbors(2),
            vec![
                PathBuf::from("f2.png"),
                PathBuf::from("f0.png"),
                PathBuf::from("f3.png"),
                PathBuf::from("f4.png"),
            ]
        );
        assert_eq!(pl.neighbors(0), Vec::<PathBuf>::new());

        // Single item -> empty
        let single = Playlist::single(PathBuf::from("only.png"));
        assert!(single.neighbors(2).is_empty());

        // 2 items -> only the other one, no duplicates
        let two = Playlist::from_files(
            vec![PathBuf::from("a.png"), PathBuf::from("b.png")],
            Path::new("a.png"),
        );
        assert_eq!(two.neighbors(5), vec![PathBuf::from("b.png")]);

        // 3 items -> +1 then -1, no duplicates even with large radius
        let three = Playlist::from_files(
            vec![
                PathBuf::from("a.png"),
                PathBuf::from("b.png"),
                PathBuf::from("c.png"),
            ],
            Path::new("a.png"),
        );
        assert_eq!(
            three.neighbors(10),
            vec![PathBuf::from("b.png"), PathBuf::from("c.png")]
        );
    }

    #[test]
    fn playlist_remove_current() {
        let mut pl = Playlist::from_files(
            vec![
                PathBuf::from("a.png"),
                PathBuf::from("b.png"),
                PathBuf::from("c.png"),
            ],
            Path::new("b.png"),
        );
        // Removing middle advances to next (c.png)
        assert_eq!(pl.remove_current(), Some(Path::new("c.png")));
        assert_eq!(pl.index(), 1);
        // Removing last moves to previous (a.png)
        assert_eq!(pl.remove_current(), Some(Path::new("a.png")));
        assert_eq!(pl.index(), 0);
        // Removing final item returns None
        assert_eq!(pl.remove_current(), None);
        assert!(pl.is_empty());
        assert_eq!(pl.remove_current(), None);
    }

    #[test]
    fn scan_folder_filters_sorts_and_includes_opened_file() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("kalux_nav_test_{}_{}", std::process::id(), unique));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir(dir.join("subdir.jpg")).unwrap();
        for name in [
            "img10.jpg",
            "img2.JPG",
            "img1.png",
            ".hidden.png",
            "notes.txt",
            "mystery.bin",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        // Open a regular image: hidden, txt, bin, and directory named .jpg are excluded.
        let pl = Playlist::scan_folder_of(&dir.join("img2.JPG")).unwrap();
        let names: Vec<String> = pl
            .files()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["img1.png", "img2.JPG", "img10.jpg"]);
        assert_eq!(pl.index(), 1);

        // Open a file with unknown extension: it is still included and selected!
        let pl2 = Playlist::scan_folder_of(&dir.join("mystery.bin")).unwrap();
        assert_eq!(pl2.current(), dir.join("mystery.bin"));
        assert_eq!(pl2.len(), 4);

        // Open a hidden file directly: it is still included and selected!
        let pl3 = Playlist::scan_folder_of(&dir.join(".hidden.png")).unwrap();
        assert_eq!(pl3.current(), dir.join(".hidden.png"));
        assert_eq!(pl3.len(), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
