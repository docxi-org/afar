//! The viewed file: read in blocks through a small cache, never loaded
//! whole. Opened sharing read, write and delete, as Far does, so other
//! programs can keep writing to it (logs) or delete it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const BLOCK: u64 = 64 * 1024;
const CACHED_BLOCKS: usize = 32;

pub struct Source {
    path: PathBuf,
    file: File,
    size: u64,
    modified: Option<SystemTime>,
    /// Most recently used first.
    cache: Vec<(u64, Box<[u8]>)>,
}

impl Source {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut options = File::options();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
            options.share_mode(7);
        }
        let file = options.open(path)?;
        let meta = file.metadata()?;
        if meta.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::IsADirectory,
                "is a directory",
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            size: meta.len(),
            modified: meta.modified().ok(),
            cache: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Checks the file for changes (Far's `CheckChanged`: size and write
    /// time); a change drops the cache. Returns whether it changed.
    pub fn refresh(&mut self) -> bool {
        let Ok(meta) = std::fs::metadata(&self.path).or_else(|_| self.file.metadata()) else {
            return false;
        };
        let (size, modified) = (meta.len(), meta.modified().ok());
        if size == self.size && modified == self.modified {
            return false;
        }
        self.size = size;
        self.modified = modified;
        self.cache.clear();
        true
    }

    fn block(&mut self, index: u64) -> &[u8] {
        if let Some(i) = self.cache.iter().position(|(b, _)| *b == index) {
            if i > 0 {
                let entry = self.cache.remove(i);
                self.cache.insert(0, entry);
            }
            return &self.cache[0].1;
        }
        let start = index * BLOCK;
        let len = self.size.saturating_sub(start).min(BLOCK) as usize;
        let mut data = vec![0u8; len];
        let read = self
            .file
            .seek(SeekFrom::Start(start))
            .and_then(|_| read_full(&mut self.file, &mut data));
        data.truncate(read.unwrap_or(0));
        self.cache.insert(0, (index, data.into_boxed_slice()));
        self.cache.truncate(CACHED_BLOCKS);
        &self.cache[0].1
    }

    /// Bytes from `pos` to the end of its block (empty at the end of the
    /// file).
    pub fn chunk(&mut self, pos: u64) -> &[u8] {
        if pos >= self.size {
            return &[];
        }
        let offset = (pos % BLOCK) as usize;
        let block = self.block(pos / BLOCK);
        block.get(offset..).unwrap_or(&[])
    }

    /// Up to `out.len()` bytes from `pos` (fewer at the end of the file).
    pub fn read(&mut self, mut pos: u64, out: &mut [u8]) -> usize {
        let mut done = 0;
        while done < out.len() {
            let chunk = self.chunk(pos);
            if chunk.is_empty() {
                break;
            }
            let n = chunk.len().min(out.len() - done);
            out[done..done + n].copy_from_slice(&chunk[..n]);
            done += n;
            pos += n as u64;
        }
        done
    }

    pub fn read_vec(&mut self, pos: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let n = self.read(pos, &mut out);
        out.truncate(n);
        out
    }

    /// The byte at `pos`.
    pub fn byte(&mut self, pos: u64) -> Option<u8> {
        self.chunk(pos).first().copied()
    }
}

fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        match file.read(&mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}
