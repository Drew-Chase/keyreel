use crate::keyboard::KeyEvent;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

pub struct KeylogWriter {
    file: BufWriter<File>,
}

impl KeylogWriter {
    pub fn new(path: &Path) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            file: BufWriter::new(file),
        })
    }

    /// Writes one JSON line and flushes immediately (crash-safety guarantee).
    pub fn write(&mut self, event: &KeyEvent) -> io::Result<()> {
        let json = serde_json::to_string(event).map_err(io::Error::other)?;
        self.file.write_all(json.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()
    }
}
