//! Line numbers of a viewed file (docs/11, "Устройство"): the agent talks
//! in lines while the viewer keeps byte offsets. Lines are counted by
//! line feeds in the file's code page, as far as asked, with a checkpoint
//! every `STEP` lines, so the next question starts near its answer.

use super::codepage::Codec;
use super::source::Source;

const STEP: u64 = 256;
/// Bytes scanned per round.
const CHUNK: u64 = 1 << 16;

#[derive(Default, Debug)]
pub struct LineIndex {
    /// The start of line `k * STEP + 1` (lines from 1).
    starts: Vec<u64>,
    /// Scanned so far: the start of line `line`.
    line: u64,
    offset: u64,
    /// The whole file is scanned (`line` is then the last line).
    done: bool,
}

impl LineIndex {
    pub fn new() -> Self {
        Self {
            starts: vec![0],
            line: 1,
            offset: 0,
            done: false,
        }
    }

    /// Scans on until `stop(line, offset)` or the end.
    fn scan(&mut self, src: &mut Source, codec: &Codec, stop: impl Fn(u64, u64) -> bool) {
        let unit = codec.unit() as u64;
        let nl = codec.encode("\n");
        let size = src.size();
        while !self.done && !stop(self.line, self.offset) {
            if self.offset >= size {
                self.done = true;
                break;
            }
            let len = CHUNK.min(size - self.offset) as usize;
            let data = src.read_vec(self.offset, len + nl.len());
            let mut i = 0usize;
            let mut advanced = false;
            while i + nl.len() <= data.len() && (i as u64) < len as u64 {
                if data[i..].starts_with(&nl) {
                    self.line += 1;
                    let start = self.offset + (i + nl.len()) as u64;
                    if (self.line - 1).is_multiple_of(STEP) {
                        self.starts.push(start);
                    }
                    if stop(self.line, start) {
                        self.offset = start;
                        advanced = true;
                        break;
                    }
                }
                i += unit as usize;
            }
            if !advanced {
                self.offset += len as u64;
            }
        }
    }

    /// Lines the file has (scanning it all).
    pub fn count(&mut self, src: &mut Source, codec: &Codec) -> u64 {
        self.scan(src, codec, |_, _| false);
        self.line
    }

    /// The byte offset where line `n` (from 1) starts; past the end — the
    /// last line's start.
    pub fn line_start(&mut self, src: &mut Source, codec: &Codec, n: u64) -> u64 {
        let n = n.max(1);
        self.scan(src, codec, |line, _| line >= n);
        let k = ((n - 1) / STEP) as usize;
        let k = k.min(self.starts.len() - 1);
        let (mut line, mut pos) = (k as u64 * STEP + 1, self.starts[k]);
        let unit = codec.unit();
        let nl = codec.encode("\n");
        while line < n {
            let data = src.read_vec(pos, CHUNK as usize);
            if data.is_empty() {
                break;
            }
            let mut i = 0;
            let mut found = false;
            while i + nl.len() <= data.len() {
                if data[i..].starts_with(&nl) {
                    line += 1;
                    pos += (i + nl.len()) as u64;
                    found = true;
                    break;
                }
                i += unit;
            }
            if !found {
                // The last line: no more line feeds.
                return pos;
            }
        }
        pos
    }

    /// The line (from 1) the byte at `offset` is on.
    pub fn line_of(&mut self, src: &mut Source, codec: &Codec, offset: u64) -> u64 {
        self.scan(src, codec, |_, o| o > offset);
        let k = match self.starts.binary_search(&offset) {
            Ok(k) => k,
            Err(k) => k.saturating_sub(1),
        };
        let line = k as u64 * STEP + 1;
        line + count_feeds(src, codec, self.starts[k], offset)
    }
}

/// Line feeds in `from..to`.
pub fn count_feeds(src: &mut Source, codec: &Codec, from: u64, to: u64) -> u64 {
    let unit = codec.unit();
    let nl = codec.encode("\n");
    let mut n = 0;
    let mut pos = from;
    while pos < to {
        let len = (to - pos).min(CHUNK) as usize;
        let data = src.read_vec(pos, len + nl.len());
        let mut i = 0;
        while i < len && i + nl.len() <= data.len() {
            if data[i..].starts_with(&nl) {
                n += 1;
            }
            i += unit;
        }
        pos += len as u64;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_offsets() {
        let dir = std::env::temp_dir().join(format!("afar-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        let text: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &text).unwrap();
        let mut src = Source::open(&path).unwrap();
        let codec = Codec::new(super::super::codepage::UTF8).unwrap();
        let mut ix = LineIndex::new();
        let start_700 = text.find("line 700\n").unwrap() as u64;
        assert_eq!(ix.line_start(&mut src, &codec, 700), start_700);
        assert_eq!(ix.line_of(&mut src, &codec, start_700 + 3), 700);
        assert_eq!(ix.line_start(&mut src, &codec, 1), 0);
        assert_eq!(ix.line_of(&mut src, &codec, 0), 1);
        // 1000 lines and the empty one after the last line feed.
        assert_eq!(ix.count(&mut src, &codec), 1001);
        drop(src);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
