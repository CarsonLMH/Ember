use image::{RgbImage, Rgba, RgbaImage};
use serde::Serialize;

/// Clipping thresholds + overlay colors. Threshold defaults follow
/// FastRawViewer's "effectively clipped" philosophy: a JPEG value of 252 is
/// as unrecoverable as 255.
#[derive(Clone, Copy, Debug)]
pub struct BlinkiesCfg {
    pub highlight: u8,
    pub shadow: u8,
    pub highlight_color: [u8; 3],
    pub shadow_color: [u8; 3],
}

impl Default for BlinkiesCfg {
    fn default() -> Self {
        Self {
            highlight: 250,
            shadow: 5,
            highlight_color: [0xFF, 0x46, 0x46],
            shadow_color: [0x50, 0x82, 0xFF],
        }
    }
}

impl BlinkiesCfg {
    /// Fingerprint for mask-cache invalidation: masks bake these values in.
    pub fn fingerprint(&self) -> String {
        format!(
            "{}-{}-{:02X?}-{:02X?}",
            self.highlight, self.shadow, self.highlight_color, self.shadow_color
        )
    }
}

#[derive(Serialize)]
pub struct Histogram {
    pub r: Vec<u32>,
    pub g: Vec<u32>,
    pub b: Vec<u32>,
    pub l: Vec<u32>,
}

/// Mask blocks are 4×4 preview pixels: visible when upscaled, cheap to encode.
const MASK_BLOCK: u32 = 4;
const MASK_ALPHA: u8 = 235;

/// One pass over the preview yields both the RGB+luminance histogram and the
/// clipping mask (highlights: ANY channel ≥ threshold — blown red in skin
/// tones counts; shadows: ALL channels ≤ threshold — truly crushed).
pub fn compute(img: &RgbImage, cfg: BlinkiesCfg) -> (Histogram, RgbaImage) {
    let (w, h) = img.dimensions();
    let mut r = vec![0u32; 256];
    let mut g = vec![0u32; 256];
    let mut b = vec![0u32; 256];
    let mut l = vec![0u32; 256];
    let mw = (w / MASK_BLOCK).max(1);
    let mh = (h / MASK_BLOCK).max(1);
    let mut mask = RgbaImage::new(mw, mh);
    let hi = Rgba([
        cfg.highlight_color[0],
        cfg.highlight_color[1],
        cfg.highlight_color[2],
        MASK_ALPHA,
    ]);
    let lo = Rgba([
        cfg.shadow_color[0],
        cfg.shadow_color[1],
        cfg.shadow_color[2],
        MASK_ALPHA,
    ]);

    for (x, y, px) in img.enumerate_pixels() {
        let [pr, pg, pb] = px.0;
        r[pr as usize] += 1;
        g[pg as usize] += 1;
        b[pb as usize] += 1;
        let luma = (0.2126 * pr as f32 + 0.7152 * pg as f32 + 0.0722 * pb as f32).round() as usize;
        l[luma.min(255)] += 1;

        let blown = pr >= cfg.highlight || pg >= cfg.highlight || pb >= cfg.highlight;
        let crushed = pr <= cfg.shadow && pg <= cfg.shadow && pb <= cfg.shadow;
        if blown || crushed {
            let mx = (x / MASK_BLOCK).min(mw - 1);
            let my = (y / MASK_BLOCK).min(mh - 1);
            mask.put_pixel(mx, my, if blown { hi } else { lo });
        }
    }
    (Histogram { r, g, b, l }, mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_counts_and_mask_marks_clipping() {
        let mut img = RgbImage::from_pixel(16, 16, image::Rgb([128, 128, 128]));
        // Blown 4×4 block top-left, crushed 4×4 block bottom-right.
        for y in 0..4 {
            for x in 0..4 {
                img.put_pixel(x, y, image::Rgb([255, 200, 100]));
                img.put_pixel(12 + x, 12 + y, image::Rgb([2, 2, 2]));
            }
        }
        let (hist, mask) = compute(&img, BlinkiesCfg::default());
        assert_eq!(hist.r.iter().sum::<u32>(), 256);
        assert_eq!(hist.r[128], 256 - 32, "midtone count");
        assert_eq!(
            mask.get_pixel(0, 0).0,
            [0xFF, 0x46, 0x46, 235],
            "highlight marked"
        );
        assert_eq!(
            mask.get_pixel(3, 3).0,
            [0x50, 0x82, 0xFF, 235],
            "shadow marked"
        );
        assert_eq!(mask.get_pixel(1, 1).0[3], 0, "clean area transparent");
    }

    #[test]
    fn custom_colors_bake_into_mask_and_fingerprint_changes() {
        let mut img = RgbImage::from_pixel(4, 4, image::Rgb([128, 128, 128]));
        img.put_pixel(0, 0, image::Rgb([255, 255, 255]));
        let cfg = BlinkiesCfg {
            highlight_color: [1, 2, 3],
            ..Default::default()
        };
        let (_, mask) = compute(&img, cfg);
        assert_eq!(mask.get_pixel(0, 0).0, [1, 2, 3, 235]);
        assert_ne!(cfg.fingerprint(), BlinkiesCfg::default().fingerprint());
    }
}
