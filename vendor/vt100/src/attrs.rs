use crate::term::BufWrite as _;

/// Represents a foreground or background color for cells.
#[derive(Eq, PartialEq, Debug, Copy, Clone, Default)]
pub enum Color {
    /// The default terminal color.
    #[default]
    Default,

    /// An indexed terminal color.
    Idx(u8),

    /// An RGB terminal color. The parameters are (red, green, blue).
    Rgb(u8, u8, u8),
}

// AFAR-PATCH begin: `mode` is u16 (blink, hidden, strikethrough,
// overline); `link` — the cell's hyperlink (OSC 8), 0: none.
const TEXT_MODE_INTENSITY: u16 = 0b0000_0011;
const TEXT_MODE_BOLD: u16 = 0b0000_0001;
const TEXT_MODE_DIM: u16 = 0b0000_0010;
const TEXT_MODE_ITALIC: u16 = 0b0000_0100;
const TEXT_MODE_UNDERLINE: u16 = 0b0000_1000;
const TEXT_MODE_INVERSE: u16 = 0b0001_0000;
const TEXT_MODE_BLINK: u16 = 0b0010_0000;
const TEXT_MODE_HIDDEN: u16 = 0b0100_0000;
const TEXT_MODE_STRIKETHROUGH: u16 = 0b1000_0000;
const TEXT_MODE_OVERLINE: u16 = 0b1_0000_0000;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attrs {
    pub fgcolor: Color,
    pub bgcolor: Color,
    pub mode: u16,
    pub link: u16,
}
// AFAR-PATCH end

impl Attrs {
    pub fn bold(&self) -> bool {
        self.mode & TEXT_MODE_BOLD != 0
    }

    pub fn dim(&self) -> bool {
        self.mode & TEXT_MODE_DIM != 0
    }

    fn intensity(&self) -> u16 {
        self.mode & TEXT_MODE_INTENSITY
    }

    pub fn set_bold(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
        self.mode |= TEXT_MODE_BOLD;
    }

    pub fn set_dim(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
        self.mode |= TEXT_MODE_DIM;
    }

    pub fn set_normal_intensity(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
    }

    pub fn italic(&self) -> bool {
        self.mode & TEXT_MODE_ITALIC != 0
    }

    pub fn set_italic(&mut self, italic: bool) {
        if italic {
            self.mode |= TEXT_MODE_ITALIC;
        } else {
            self.mode &= !TEXT_MODE_ITALIC;
        }
    }

    pub fn underline(&self) -> bool {
        self.mode & TEXT_MODE_UNDERLINE != 0
    }

    pub fn set_underline(&mut self, underline: bool) {
        if underline {
            self.mode |= TEXT_MODE_UNDERLINE;
        } else {
            self.mode &= !TEXT_MODE_UNDERLINE;
        }
    }

    pub fn inverse(&self) -> bool {
        self.mode & TEXT_MODE_INVERSE != 0
    }

    pub fn set_inverse(&mut self, inverse: bool) {
        if inverse {
            self.mode |= TEXT_MODE_INVERSE;
        } else {
            self.mode &= !TEXT_MODE_INVERSE;
        }
    }

    // AFAR-PATCH begin
    fn set_mode(&mut self, bit: u16, on: bool) {
        if on {
            self.mode |= bit;
        } else {
            self.mode &= !bit;
        }
    }

    pub fn blink(&self) -> bool {
        self.mode & TEXT_MODE_BLINK != 0
    }

    pub fn set_blink(&mut self, on: bool) {
        self.set_mode(TEXT_MODE_BLINK, on);
    }

    pub fn hidden(&self) -> bool {
        self.mode & TEXT_MODE_HIDDEN != 0
    }

    pub fn set_hidden(&mut self, on: bool) {
        self.set_mode(TEXT_MODE_HIDDEN, on);
    }

    pub fn strikethrough(&self) -> bool {
        self.mode & TEXT_MODE_STRIKETHROUGH != 0
    }

    pub fn set_strikethrough(&mut self, on: bool) {
        self.set_mode(TEXT_MODE_STRIKETHROUGH, on);
    }

    pub fn overline(&self) -> bool {
        self.mode & TEXT_MODE_OVERLINE != 0
    }

    pub fn set_overline(&mut self, on: bool) {
        self.set_mode(TEXT_MODE_OVERLINE, on);
    }

    /// SGR 0: everything but the hyperlink (OSC 8 is not SGR).
    pub fn reset_keeping_link(&mut self) {
        *self = Self {
            link: self.link,
            ..Self::default()
        };
    }
    // AFAR-PATCH end

    // The new modes and links are not written (afar does not use the
    // formatted output).
    pub fn write_escape_code_diff(
        &self,
        contents: &mut Vec<u8>,
        other: &Self,
    ) {
        if self != other && self == &Self::default() {
            crate::term::ClearAttrs.write_buf(contents);
            return;
        }

        let attrs = crate::term::Attrs::default();

        let attrs = if self.fgcolor == other.fgcolor {
            attrs
        } else {
            attrs.fgcolor(self.fgcolor)
        };
        let attrs = if self.bgcolor == other.bgcolor {
            attrs
        } else {
            attrs.bgcolor(self.bgcolor)
        };
        let attrs = if self.intensity() == other.intensity() {
            attrs
        } else {
            attrs.intensity(match self.intensity() {
                0 => crate::term::Intensity::Normal,
                TEXT_MODE_BOLD => crate::term::Intensity::Bold,
                TEXT_MODE_DIM => crate::term::Intensity::Dim,
                _ => unreachable!(),
            })
        };
        let attrs = if self.italic() == other.italic() {
            attrs
        } else {
            attrs.italic(self.italic())
        };
        let attrs = if self.underline() == other.underline() {
            attrs
        } else {
            attrs.underline(self.underline())
        };
        let attrs = if self.inverse() == other.inverse() {
            attrs
        } else {
            attrs.inverse(self.inverse())
        };

        attrs.write_buf(contents);
    }
}
