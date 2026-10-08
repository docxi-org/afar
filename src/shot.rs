//! Snapshots of afar's screen for the agent's test tools
//! (`afar_test_screen`): the last frame as text with its colors, or as a
//! PNG picture — glyphs from the Windows console font (Consolas), box
//! drawing and blocks drawn as lines so frames join, colors of Windows
//! Terminal's default scheme (Campbell).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier};

// ------------------------------------------------------------------ text

/// A color's name: Far's console names for the 16 colors, `default` for
/// the terminal's own, `iN` / `#rrggbb` otherwise.
pub fn color_name(c: Color) -> String {
    match c {
        Color::Reset => "default".into(),
        Color::Black => "black".into(),
        Color::Red => "red".into(),
        Color::Green => "green".into(),
        Color::Yellow => "brown".into(),
        Color::Blue => "blue".into(),
        Color::Magenta => "magenta".into(),
        Color::Cyan => "cyan".into(),
        Color::Gray => "lightgray".into(),
        Color::DarkGray => "darkgray".into(),
        Color::LightRed => "lightred".into(),
        Color::LightGreen => "lightgreen".into(),
        Color::LightYellow => "yellow".into(),
        Color::LightBlue => "lightblue".into(),
        Color::LightMagenta => "lightmagenta".into(),
        Color::LightCyan => "lightcyan".into(),
        Color::White => "white".into(),
        Color::Indexed(n) => format!("i{n}"),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn style_name(fg: Color, bg: Color, m: Modifier) -> String {
    let mut s = format!("{}/{}", color_name(fg), color_name(bg));
    for (flag, name) in [
        (Modifier::BOLD, "bold"),
        (Modifier::UNDERLINED, "underline"),
        (Modifier::REVERSED, "reverse"),
        (Modifier::ITALIC, "italic"),
        (Modifier::DIM, "dim"),
        (Modifier::CROSSED_OUT, "struck"),
    ] {
        if m.contains(flag) {
            s.push('+');
            s.push_str(name);
        }
    }
    s
}

/// The screen's `rows` as text: numbered rows (numbers of the whole
/// screen), then for each row its runs of colors (`fg/bg` by columns), and
/// the cursor.
pub fn text(buf: &Buffer, cursor: Option<Position>, rows: Range<u16>) -> String {
    let a = buf.area;
    let rows = rows.start.max(a.top())..rows.end.min(a.bottom());
    let mut out = format!(
        "[afar screen {}×{}, rows {}-{} shown; rows and columns from 0; cursor {}]\n",
        a.width,
        a.height,
        rows.start,
        rows.end.saturating_sub(1),
        cursor.map_or("hidden".to_string(), |p| format!("{},{}", p.y, p.x)),
    );
    for y in rows.clone() {
        let mut line = String::new();
        for x in a.left()..a.right() {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(&format!("{y:>3}|{}\n", line.trim_end()));
    }
    out.push_str("colors (columns fg/bg):\n");
    for y in rows {
        let mut runs: Vec<(u16, u16, String)> = Vec::new();
        for x in a.left()..a.right() {
            let c = &buf[(x, y)];
            let name = style_name(c.fg, c.bg, c.modifier);
            match runs.last_mut() {
                Some(r) if r.2 == name => r.1 = x,
                _ => runs.push((x, x, name)),
            }
        }
        let parts: Vec<String> = runs
            .iter()
            .map(|(a, b, n)| {
                if a == b {
                    format!("{a} {n}")
                } else {
                    format!("{a}-{b} {n}")
                }
            })
            .collect();
        out.push_str(&format!("{y:>3}: {}\n", parts.join("; ")));
    }
    out
}

// ------------------------------------------------------------------- png

/// Windows Terminal's Campbell scheme, in the console's order of names.
fn rgb(c: Color, fg: bool) -> [u8; 3] {
    const ANSI: [[u8; 3]; 16] = [
        [0x0C, 0x0C, 0x0C],
        [0xC5, 0x0F, 0x1F],
        [0x13, 0xA1, 0x0E],
        [0xC1, 0x9C, 0x00],
        [0x00, 0x37, 0xDA],
        [0x88, 0x17, 0x98],
        [0x3A, 0x96, 0xDD],
        [0xCC, 0xCC, 0xCC],
        [0x76, 0x76, 0x76],
        [0xE7, 0x48, 0x56],
        [0x16, 0xC6, 0x0C],
        [0xF9, 0xF1, 0xA5],
        [0x3B, 0x78, 0xFF],
        [0xB4, 0x00, 0x9E],
        [0x61, 0xD6, 0xD6],
        [0xF2, 0xF2, 0xF2],
    ];
    let index = |n: u8| -> [u8; 3] {
        match n {
            0..=15 => ANSI[usize::from(n)],
            16..=231 => {
                let n = n - 16;
                let v = |k: u8| if k == 0 { 0 } else { 55 + k * 40 };
                [v(n / 36), v(n / 6 % 6), v(n % 6)]
            }
            _ => {
                let g = 8 + (n - 232) * 10;
                [g, g, g]
            }
        }
    };
    match c {
        Color::Reset => {
            if fg {
                ANSI[7]
            } else {
                ANSI[0]
            }
        }
        Color::Black => ANSI[0],
        Color::Red => ANSI[1],
        Color::Green => ANSI[2],
        Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5],
        Color::Cyan => ANSI[6],
        Color::Gray => ANSI[7],
        Color::DarkGray => ANSI[8],
        Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10],
        Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12],
        Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14],
        Color::White => ANSI[15],
        Color::Indexed(n) => index(n),
        Color::Rgb(r, g, b) => [r, g, b],
    }
}

/// A rasterized glyph: its metrics and coverage.
type Glyph = (fontdue::Metrics, Vec<u8>);

struct Fonts {
    regular: fontdue::Font,
    bold: Option<fontdue::Font>,
    fallback: Vec<fontdue::Font>,
    glyphs: Mutex<HashMap<(char, bool), Glyph>>,
}

const PX: f32 = 16.0;

fn fonts() -> Result<&'static Fonts, String> {
    static FONTS: OnceLock<Result<Fonts, String>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let dir = std::env::var_os("WINDIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "C:\\Windows".into())
                .join("Fonts");
            let load = |name: &str| -> Option<fontdue::Font> {
                let data = std::fs::read(dir.join(name)).ok()?;
                fontdue::Font::from_bytes(data, fontdue::FontSettings::default()).ok()
            };
            let regular =
                load("consola.ttf").ok_or("no Consolas font (consola.ttf) to draw with")?;
            Ok(Fonts {
                regular,
                bold: load("consolab.ttf"),
                fallback: ["seguisym.ttf", "segoeui.ttf", "msyh.ttc", "simsun.ttc"]
                    .iter()
                    .filter_map(|n| load(n))
                    .collect(),
                glyphs: Mutex::new(HashMap::new()),
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl Fonts {
    fn glyph(&self, c: char, bold: bool) -> Glyph {
        let mut cache = self.glyphs.lock().unwrap();
        if let Some(g) = cache.get(&(c, bold)) {
            return g.clone();
        }
        let base = if bold {
            self.bold.as_ref().unwrap_or(&self.regular)
        } else {
            &self.regular
        };
        let font = if base.lookup_glyph_index(c) != 0 {
            base
        } else {
            self.fallback
                .iter()
                .find(|f| f.lookup_glyph_index(c) != 0)
                .unwrap_or(base)
        };
        let g = font.rasterize(c, PX);
        cache.insert((c, bold), g.clone());
        g
    }
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Canvas {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: [u8; 3]) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                let i = (yy * self.w + xx) * 3;
                self.px[i..i + 3].copy_from_slice(&c);
            }
        }
    }

    fn blend(&mut self, x: isize, y: isize, cov: u8, c: [u8; 3]) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h || cov == 0 {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 3;
        let a = u16::from(cov);
        for (p, c) in self.px[i..i + 3].iter_mut().zip(c) {
            *p = ((u16::from(c) * a + u16::from(*p) * (255 - a)) / 255) as u8;
        }
    }
}

/// Box drawing: the weight of each arm (left, right, up, down): 0 none,
/// 1 single, 2 double; and shaded / solid blocks.
fn box_arms(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' => [1, 1, 0, 0],
        '│' => [0, 0, 1, 1],
        '┌' => [0, 1, 0, 1],
        '┐' => [1, 0, 0, 1],
        '└' => [0, 1, 1, 0],
        '┘' => [1, 0, 1, 0],
        '├' => [0, 1, 1, 1],
        '┤' => [1, 0, 1, 1],
        '┬' => [1, 1, 0, 1],
        '┴' => [1, 1, 1, 0],
        '┼' => [1, 1, 1, 1],
        '═' => [2, 2, 0, 0],
        '║' => [0, 0, 2, 2],
        '╔' => [0, 2, 0, 2],
        '╗' => [2, 0, 0, 2],
        '╚' => [0, 2, 2, 0],
        '╝' => [2, 0, 2, 0],
        '╠' => [0, 2, 2, 2],
        '╣' => [2, 0, 2, 2],
        '╦' => [2, 2, 0, 2],
        '╩' => [2, 2, 2, 0],
        '╬' => [2, 2, 2, 2],
        '╟' => [0, 1, 2, 2],
        '╢' => [1, 0, 2, 2],
        '╤' => [2, 2, 0, 1],
        '╧' => [2, 2, 1, 0],
        '╞' => [0, 2, 1, 1],
        '╡' => [2, 0, 1, 1],
        '╥' => [1, 1, 0, 2],
        '╨' => [1, 1, 2, 0],
        '╒' => [0, 2, 0, 1],
        '╕' => [2, 0, 0, 1],
        '╘' => [0, 2, 1, 0],
        '╛' => [2, 0, 1, 0],
        '╓' => [0, 1, 0, 2],
        '╖' => [1, 0, 0, 2],
        '╙' => [0, 1, 2, 0],
        '╜' => [1, 0, 2, 0],
        _ => return None,
    })
}

fn draw_box(cv: &mut Canvas, x: usize, y: usize, w: usize, h: usize, arms: [u8; 4], c: [u8; 3]) {
    let (cx, cy) = (x + w / 2, y + h / 2);
    let gap = 2;
    // Horizontal arms.
    for (arm, from, to) in [(arms[0], x, cx + 1), (arms[1], cx, x + w)] {
        match arm {
            1 => cv.fill(from, cy, to - from, 1, c),
            2 => {
                cv.fill(from, cy - gap, to - from, 1, c);
                cv.fill(from, cy + gap, to - from, 1, c);
            }
            _ => {}
        }
    }
    for (arm, from, to) in [(arms[2], y, cy + 1), (arms[3], cy, y + h)] {
        match arm {
            1 => cv.fill(cx, from, 1, to - from, c),
            2 => {
                cv.fill(cx - gap, from, 1, to - from, c);
                cv.fill(cx + gap, from, 1, to - from, c);
            }
            _ => {}
        }
    }
}

/// The screen's `rows` as a PNG picture.
pub fn png(buf: &Buffer, cursor: Option<Position>, rows: Range<u16>) -> Result<Vec<u8>, String> {
    let f = fonts()?;
    let cell_w = f.regular.metrics('M', PX).advance_width.ceil() as usize;
    let lm = f
        .regular
        .horizontal_line_metrics(PX)
        .ok_or("the font has no line metrics")?;
    let cell_h = (lm.ascent - lm.descent + lm.line_gap).ceil() as usize;
    let baseline = lm.ascent.round() as isize;
    let full = buf.area;
    let rows = rows.start.max(full.top())..rows.end.min(full.bottom());
    let a = ratatui::layout::Rect::new(full.x, rows.start, full.width, rows.end - rows.start);
    let mut cv = Canvas {
        w: usize::from(a.width) * cell_w,
        h: usize::from(a.height) * cell_h,
        px: vec![0; usize::from(a.width) * cell_w * usize::from(a.height) * cell_h * 3],
    };
    for row in 0..a.height {
        let mut skip = 0;
        for col in 0..a.width {
            let cell = &buf[(a.x + col, a.y + row)];
            let (x, y) = (usize::from(col) * cell_w, usize::from(row) * cell_h);
            let mut fg = rgb(cell.fg, true);
            let mut bg = rgb(cell.bg, false);
            let at_cursor = cursor == Some(Position::new(a.x + col, a.y + row));
            if cell.modifier.contains(Modifier::REVERSED) != at_cursor {
                std::mem::swap(&mut fg, &mut bg);
            }
            cv.fill(x, y, cell_w, cell_h, bg);
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let Some(ch) = cell.symbol().chars().next() else {
                continue;
            };
            let wide = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) > 1;
            if wide {
                skip = 1;
            }
            if let Some(arms) = box_arms(ch) {
                draw_box(&mut cv, x, y, cell_w, cell_h, arms, fg);
            } else {
                match ch {
                    ' ' => {}
                    '█' => cv.fill(x, y, cell_w, cell_h, fg),
                    '▀' => cv.fill(x, y, cell_w, cell_h / 2, fg),
                    '▄' => cv.fill(x, y + cell_h / 2, cell_w, cell_h - cell_h / 2, fg),
                    '░' | '▒' | '▓' => {
                        let step = match ch {
                            '░' => 3,
                            '▒' => 2,
                            _ => 1,
                        };
                        for yy in 0..cell_h {
                            for xx in 0..cell_w {
                                if (xx + yy) % (step + 1) == 0 {
                                    cv.fill(x + xx, y + yy, 1, 1, fg);
                                }
                            }
                        }
                    }
                    _ => {
                        let bold = cell.modifier.contains(Modifier::BOLD);
                        let (m, bitmap) = f.glyph(ch, bold);
                        let gx = x as isize + m.xmin as isize;
                        let gy = y as isize + baseline - m.height as isize - m.ymin as isize;
                        for yy in 0..m.height {
                            for xx in 0..m.width {
                                cv.blend(
                                    gx + xx as isize,
                                    gy + yy as isize,
                                    bitmap[yy * m.width + xx],
                                    fg,
                                );
                            }
                        }
                    }
                }
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                cv.fill(x, y + cell_h - 2, cell_w * if wide { 2 } else { 1 }, 1, fg);
            }
            if cell.modifier.contains(Modifier::CROSSED_OUT) {
                cv.fill(x, y + cell_h / 2, cell_w * if wide { 2 } else { 1 }, 1, fg);
            }
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, cv.w as u32, cv.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(&cv.px).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Standard base64 (for the image in an MCP answer).
pub fn base64(data: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn text_has_rows_and_color_runs() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 2));
        buf.set_string(
            0,
            0,
            "ab",
            Style::new().fg(Color::LightCyan).bg(Color::Blue),
        );
        let t = text(&buf, Some(Position::new(1, 0)), 0..2);
        assert!(t.contains("  0|ab"));
        assert!(t.contains("0-1 lightcyan/blue; 2-5 default/default"));
        assert!(t.contains("cursor 0,1"));
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
    }
}
