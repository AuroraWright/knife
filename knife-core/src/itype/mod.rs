//! Monotype iType as the Switch SDK builds it (nn::fontll, the scalable font engine nn::font's glyph caches
//! render with), for glyph bitmaps that match the console's: its scan converter, outline effect and the
//! TrueType glyph scaling and autohinting it applies on the way.

pub mod cff;
pub mod effect;
pub mod fixed;
pub mod fnt;
pub mod glyph;
pub mod raster;
pub mod rtgah;
pub mod tt;
