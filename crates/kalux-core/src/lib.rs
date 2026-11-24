//! Platform-independent core of Kalux.
//!
//! Everything here must build and test on Linux (`cargo test -p kalux-core`).
//! No `windows` crate, no I/O beyond `std::fs` directory listing.
//!
//! The public items in this crate are the **contract** between `kalux-decode`
//! and `kalux-app`. Changing a signature requires updating both consumers.

pub mod action;
pub mod cache;
pub mod image;
pub mod nav;
pub mod theme;
pub mod view;

pub use action::{Action, Modifiers, action_for_key};
pub use cache::{ByteLru, Weighted};
pub use image::{DecodedImage, Frame, ImageFormat, SUPPORTED_EXTENSIONS};
pub use nav::{Playlist, natural_cmp};
pub use theme::{Palette, Rgba};
pub use view::{Rect, View, ZoomMode};
