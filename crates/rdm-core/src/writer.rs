//! Positioned writes into a single (sparse where supported) output file.
//!
//! Every worker writes its bytes directly at their final offset, so there is
//! no part-file concatenation step. These calls block, so callers run them
//! on Tokio's blocking pool.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

#[derive(Debug)]
pub(crate) struct OutputFile {
    file: File,
    path: PathBuf,
}

impl OutputFile {
    /// Opens (creating if needed) the output file. `truncate` discards any
    /// existing contents.
    pub fn open(path: &Path, truncate: bool) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(truncate)
            .open(path)
            .map_err(Error::io(path))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Sets the file length. Growing a file this way is sparse on
    /// filesystems that support it (APFS, ext4, ...). On NTFS the first write
    /// far past the valid-data length makes Windows zero-fill the gap.
    pub fn set_len(&self, len: u64) -> Result<()> {
        self.file.set_len(len).map_err(Error::io(&self.path))
    }

    pub fn len(&self) -> Result<u64> {
        Ok(self.file.metadata().map_err(Error::io(&self.path))?.len())
    }

    pub fn write_all_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        write_all_at(&self.file, buf, offset).map_err(Error::io(&self.path))
    }

    /// Flushes file data (not necessarily metadata) to stable storage.
    pub fn sync_data(&self) -> Result<()> {
        self.file.sync_data().map_err(Error::io(&self.path))
    }
}

#[cfg(unix)]
fn write_all_at(file: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::windows::fs::FileExt;
    // `seek_write` passes the offset in an OVERLAPPED structure, so concurrent
    // calls on the same handle do not race on the shared file cursor.
    while !buf.is_empty() {
        match file.seek_write(buf, offset) {
            Ok(0) => return Err(Error::from(ErrorKind::WriteZero)),
            Ok(n) => {
                buf = buf.get(n..).unwrap_or_default();
                offset += n as u64;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
compile_error!("rdm-core supports Unix and Windows targets only");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_positioned_writes() {
        let dir = std::env::temp_dir().join(format!("rdm-writer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.bin");
        let f = OutputFile::open(&path, true).unwrap();
        f.set_len(12).unwrap();
        f.write_all_at(b"world", 7).unwrap();
        f.write_all_at(b"hello, ", 0).unwrap();
        f.sync_data().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello, world");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
