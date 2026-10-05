//! Tray icons: a cube in the colour of the core's state, drawn at the sizes
//! tray hosts pick from, so no image files or decoders are needed.
//! `contrib/singbox-board.svg` is the same cube in the application colour.

use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Running,
    /// Starting, stopping, restarting or a request in progress.
    Busy,
    Stopped,
    /// sing-box failed, or the daemon cannot be reached.
    Error,
}

impl Tone {
    const ALL: [Tone; 4] = [Tone::Running, Tone::Busy, Tone::Stopped, Tone::Error];

    /// Breeze's positive, neutral, inactive and negative colours.
    fn rgb(self) -> u32 {
        match self {
            Tone::Running => 0x27ae60,
            Tone::Busy => 0xf67400,
            Tone::Stopped => 0x7f8c8d,
            Tone::Error => 0xda4453,
        }
    }
}

/// Sizes of the panel icons; hosts scale the closest one.
const SIZES: [u32; 7] = [16, 22, 24, 32, 44, 48, 64];

/// The icon in every size, as StatusNotifierItem pixmaps.
pub fn pixmaps(tone: Tone) -> Vec<ksni::Icon> {
    static ICONS: LazyLock<Vec<Vec<ksni::Icon>>> = LazyLock::new(|| {
        Tone::ALL
            .iter()
            .map(|&tone| {
                SIZES
                    .iter()
                    .map(|&size| {
                        let mut data = rgba(tone, size);
                        for pixel in data.as_chunks_mut::<4>().0 {
                            pixel.rotate_right(1);
                        }
                        ksni::Icon {
                            width: size as i32,
                            height: size as i32,
                            data,
                        }
                    })
                    .collect()
            })
            .collect()
    });
    ICONS[tone as usize].clone()
}

/// The icon as non-premultiplied RGBA rows of `size` pixels.
pub fn rgba(tone: Tone, size: u32) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let base = channels(tone.rgb());
    // Light from above: a light top, the base colour left, a shaded right.
    let shades = [mix(base, 1.0, 0.35), base, mix(base, 0.0, 0.25)];
    // A gap of about one pixel between the faces, wider on large icons.
    let gap = (size as f32 * 0.045).max(1.0) / size as f32;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut sum = [0.0f32; 3];
            let mut hits = 0u32;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let py = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    if let Some(face) = face_at(px, py, gap) {
                        for (total, value) in sum.iter_mut().zip(shades[face]) {
                            *total += value;
                        }
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                data.extend([0, 0, 0, 0]);
                continue;
            }
            let [r, g, b] = sum.map(|total| byte(total / hits as f32));
            data.extend([r, g, b, byte(hits as f32 / (SAMPLES * SAMPLES) as f32)]);
        }
    }
    data
}

type Point = (f32, f32);

/// A pointy-top hexagon in the unit square, split into the top, left and
/// right faces of a cube.
const CENTER: Point = (0.5, 0.5);
const RADIUS: f32 = 0.48;
const HALF_WIDTH: f32 = RADIUS * 0.866_025_4;
const TOP: Point = (0.5, 0.5 - RADIUS);
const UPPER_RIGHT: Point = (0.5 + HALF_WIDTH, 0.5 - RADIUS / 2.0);
const LOWER_RIGHT: Point = (0.5 + HALF_WIDTH, 0.5 + RADIUS / 2.0);
const BOTTOM: Point = (0.5, 0.5 + RADIUS);
const LOWER_LEFT: Point = (0.5 - HALF_WIDTH, 0.5 + RADIUS / 2.0);
const UPPER_LEFT: Point = (0.5 - HALF_WIDTH, 0.5 - RADIUS / 2.0);
const FACES: [[Point; 4]; 3] = [
    [TOP, UPPER_RIGHT, CENTER, UPPER_LEFT],
    [UPPER_LEFT, CENTER, BOTTOM, LOWER_LEFT],
    [CENTER, UPPER_RIGHT, LOWER_RIGHT, BOTTOM],
];
/// The edges between the faces, kept clear by the gap.
const INNER_EDGES: [Point; 3] = [UPPER_LEFT, UPPER_RIGHT, BOTTOM];

fn face_at(x: f32, y: f32, gap: f32) -> Option<usize> {
    if INNER_EDGES
        .iter()
        .any(|&end| distance_to_segment((x, y), CENTER, end) < gap / 2.0)
    {
        return None;
    }
    FACES.iter().position(|face| contains(face, (x, y)))
}

/// Whether the convex polygon `corners` (in either winding) contains `p`.
fn contains(corners: &[Point], p: Point) -> bool {
    let mut sign = 0.0f32;
    for (i, &a) in corners.iter().enumerate() {
        let b = corners[(i + 1) % corners.len()];
        let cross = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return false;
            }
            sign = cross.signum();
        }
    }
    true
}

fn distance_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

fn channels(rgb: u32) -> [f32; 3] {
    [16, 8, 0].map(|shift| ((rgb >> shift) & 0xff) as f32 / 255.0)
}

/// `color` moved towards the grey level `target` by `amount`.
fn mix(color: [f32; 3], target: f32, amount: f32) -> [f32; 3] {
    color.map(|c| c + (target - c) * amount)
}

fn byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(data: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * size + x) * 4) as usize;
        data[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn faces_are_shaded_and_corners_transparent() {
        let size = 64;
        let data = rgba(Tone::Running, size);
        assert_eq!(data.len(), (size * size * 4) as usize);
        assert_eq!(pixel(&data, size, 0, 0)[3], 0);
        assert_eq!(pixel(&data, size, 63, 63)[3], 0);
        let top = pixel(&data, size, 32, 10);
        let left = pixel(&data, size, 18, 40);
        let right = pixel(&data, size, 46, 40);
        for face in [top, left, right] {
            assert_eq!(face[3], 255, "{face:?}");
        }
        assert_eq!(&left[..3], &[0x27, 0xae, 0x60]);
        let brightness = |p: [u8; 4]| p[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        assert!(brightness(top) > brightness(left));
        assert!(brightness(left) > brightness(right));
        // The gap between the faces is see-through.
        assert!(pixel(&data, size, 32, 50)[3] < 128);
    }

    #[test]
    fn pixmaps_are_argb_in_every_size() {
        let icons = pixmaps(Tone::Error);
        assert_eq!(icons.len(), SIZES.len());
        for (icon, size) in icons.iter().zip(SIZES) {
            assert_eq!((icon.width, icon.height), (size as i32, size as i32));
            assert_eq!(icon.data.len(), (size * size * 4) as usize);
        }
        let large = icons.last().unwrap();
        let at = ((40 * 64 + 18) * 4) as usize;
        assert_eq!(&large.data[at..at + 4], &[255, 0xda, 0x44, 0x53]);
    }
}
