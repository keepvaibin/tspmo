//! The pixel the software renderer draws into: premultiplied, transparent where nothing is drawn.

use slint::platform::software_renderer::{PremultipliedRgbaColor, TargetPixel};

#[derive(Clone, Copy, Default)]
pub struct Px {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl TargetPixel for Px {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let ia = u16::from(255 - c.alpha);
        self.r = ((u16::from(self.r) * ia + 127) / 255) as u8 + c.red;
        self.g = ((u16::from(self.g) * ia + 127) / 255) as u8 + c.green;
        self.b = ((u16::from(self.b) * ia + 127) / 255) as u8 + c.blue;
        self.a = ((u16::from(self.a) * ia + 127) / 255) as u8 + c.alpha;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Px { r, g, b, a: 255 }
    }
    fn background() -> Self {
        Px::default()
    }
}
