//! Tray icon: the minutes (or hours) left, drawn on a colored badge.
//! Plain pixel pushing with a 3x5 bitmap font; no image or font crates.

/// ARGB32 pixels in network byte order, as StatusNotifierItem expects.
pub struct Pixmap {
    pub size: i32,
    pub data: Vec<u8>,
}

const SIZE: usize = 32;

/// Rows of 3 pixels, most significant of the low 3 bits is the left pixel.
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        'h' => [0b100, 0b100, 0b111, 0b101, 0b101],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        _ => [0b000, 0b000, 0b010, 0b000, 0b000],
    }
}

/// Badge text for the time left: "45" (minutes) below an hour, "2h" above.
pub fn label(remaining_secs: u64) -> String {
    let min = remaining_secs.div_ceil(60);
    if min < 60 {
        min.to_string()
    } else {
        format!("{}h", min / 60)
    }
}

/// (background, foreground) as (r, g, b): green, amber, red as time runs out.
fn colors(remaining_secs: u64) -> ([u8; 3], [u8; 3]) {
    match remaining_secs {
        s if s > 15 * 60 => ([0x2e, 0x7d, 0x32], [0xff, 0xff, 0xff]),
        s if s > 5 * 60 => ([0xf9, 0xa8, 0x25], [0x20, 0x20, 0x20]),
        _ => ([0xc6, 0x28, 0x28], [0xff, 0xff, 0xff]),
    }
}

/// `unlimited` (limits off for today) shows "--" on green.
pub fn render(remaining_secs: u64, unlimited: bool) -> Pixmap {
    let text = if unlimited {
        "--".to_string()
    } else {
        label(remaining_secs)
    };
    let (bg, fg) = colors(if unlimited { u64::MAX } else { remaining_secs });
    let mut px = vec![[0u8; 4]; SIZE * SIZE]; // [a, r, g, b]

    // Rounded-square badge.
    let radius = 7i32;
    let max = SIZE as i32 - 1;
    for y in 0..SIZE as i32 {
        for x in 0..SIZE as i32 {
            let cx = x.clamp(radius, max - radius);
            let cy = y.clamp(radius, max - radius);
            let (dx, dy) = (x - cx, y - cy);
            if dx * dx + dy * dy <= radius * radius {
                px[y as usize * SIZE + x as usize] = [0xff, bg[0], bg[1], bg[2]];
            }
        }
    }

    // Text, scaled to fit: 4x for up to two characters, 3x for three.
    let n = text.chars().count();
    let scale = if n <= 2 { 4 } else { 3 };
    let gap = scale / 2;
    let width = n * 3 * scale + (n.saturating_sub(1)) * gap;
    let height = 5 * scale;
    let x0 = SIZE.saturating_sub(width) / 2;
    let y0 = (SIZE - height) / 2;
    for (i, c) in text.chars().enumerate() {
        let gx = x0 + i * (3 * scale + gap);
        for (row, bits) in glyph(c).iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        let x = gx + col * scale + sx;
                        let y = y0 + row * scale + sy;
                        if x < SIZE && y < SIZE {
                            px[y * SIZE + x] = [0xff, fg[0], fg[1], fg[2]];
                        }
                    }
                }
            }
        }
    }

    Pixmap {
        size: SIZE as i32,
        data: px.into_iter().flatten().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(label(0), "0");
        assert_eq!(label(1), "1");
        assert_eq!(label(45 * 60), "45");
        assert_eq!(label(59 * 60 + 1), "1h");
        assert_eq!(label(150 * 60), "2h");
        assert_eq!(label(24 * 3600), "24h");
    }

    #[test]
    fn renders_full_size() {
        for (secs, unlimited) in [
            (0, false),
            (59, false),
            (45 * 60, false),
            (3 * 3600, false),
            (24 * 3600, false),
            (0, true),
        ] {
            let p = render(secs, unlimited);
            assert_eq!(p.data.len(), SIZE * SIZE * 4);
            // Corner is transparent, center of badge is opaque.
            assert_eq!(p.data[0], 0);
            assert_eq!(p.data[(SIZE * SIZE / 2 + 2) * 4], 0xff);
        }
    }
}
