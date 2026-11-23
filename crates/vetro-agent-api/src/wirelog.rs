use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, Write},
    mem::size_of,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use vetro_proto::{RawDirection, RawEntry};

struct LogState {
    entries: VecDeque<RawEntry>,
    bytes: usize,
    file: Option<File>,
    file_error: Option<String>,
}

pub struct WireLog {
    max_bytes: usize,
    state: Mutex<LogState>,
    filter: fn(&str) -> bool,
}

impl Default for WireLog {
    fn default() -> Self {
        Self::new(4 * 1024 * 1024)
    }
}

impl WireLog {
    pub fn new(max_bytes: usize) -> Self {
        Self::build(max_bytes, None)
    }

    pub fn with_file(max_bytes: usize, path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self::build(max_bytes, Some(file)))
    }

    fn build(max_bytes: usize, file: Option<File>) -> Self {
        Self {
            max_bytes,
            filter: |_| true,
            state: Mutex::new(LogState {
                entries: VecDeque::new(),
                bytes: 0,
                file,
                file_error: None,
            }),
        }
    }

    pub fn with_filter(mut self, filter: fn(&str) -> bool) -> Self {
        self.filter = filter;
        self
    }

    pub fn snapshot(&self) -> Vec<RawEntry> {
        self.state.lock().unwrap().entries.iter().cloned().collect()
    }

    pub fn file_error(&self) -> Option<String> {
        self.state.lock().unwrap().file_error.clone()
    }

    pub(crate) fn record(&self, direction: RawDirection, mut text: String) {
        if !(self.filter)(&text) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        text.shrink_to_fit();
        let entry = RawEntry {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
            direction,
            text,
        };
        if let Some(file) = state.file.as_mut() {
            if let Err(error) = append(file, &entry) {
                state.file_error = Some(error.to_string());
                state.file = None;
            }
        }

        let bytes = entry_size(&entry);
        if bytes > self.max_bytes {
            return;
        }
        while state.bytes > self.max_bytes - bytes {
            let oldest = state.entries.pop_front().unwrap();
            state.bytes -= entry_size(&oldest);
        }
        state.bytes += bytes;
        state.entries.push_back(entry);
    }
}

fn entry_size(entry: &RawEntry) -> usize {
    size_of::<RawEntry>() + entry.text.capacity()
}

fn append(file: &mut File, entry: &RawEntry) -> io::Result<()> {
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    file.write_all(&line)
}
