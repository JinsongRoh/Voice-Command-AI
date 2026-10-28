//! Incremental reading of JSONL transcript files.
//!
//! [`JsonlTail`] remembers how far a file has been read and turns newly
//! appended complete lines into [`HistoryItem`]s. [`spawn_tail`] drives it
//! from a background task that wakes up on file system notifications and,
//! as a fallback, on a fixed polling interval.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use tokio::sync::{Notify, mpsc};

use crate::types::HistoryItem;

/// Converts one transcript line into zero or more history items. Parsers
/// may keep state across lines (for example session metadata).
pub trait LineParser: Send + 'static {
    fn parse_line(&mut self, line: &str) -> Vec<HistoryItem>;
}

impl<F> LineParser for F
where
    F: FnMut(&str) -> Vec<HistoryItem> + Send + 'static,
{
    fn parse_line(&mut self, line: &str) -> Vec<HistoryItem> {
        self(line)
    }
}

/// Reads a JSONL file incrementally.
pub struct JsonlTail<P> {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    parser: P,
}

impl<P: LineParser> JsonlTail<P> {
    /// Starts reading at the beginning of the file.
    pub fn new(path: impl Into<PathBuf>, parser: P) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            partial: Vec::new(),
            parser,
        }
    }

    /// Starts reading at the current end of the file, so only lines
    /// appended from now on are reported.
    pub fn from_end(path: impl Into<PathBuf>, parser: P) -> io::Result<Self> {
        let path = path.into();
        let offset = match std::fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
            Err(err) => return Err(err),
        };
        Ok(Self {
            path,
            offset,
            partial: Vec::new(),
            parser,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads everything appended since the last call. A missing file yields
    /// nothing; a truncated file is read again from the start. An incomplete
    /// last line is kept until its newline arrives.
    pub fn read_new(&mut self) -> io::Result<Vec<HistoryItem>> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let len = file.metadata()?.len();
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::with_capacity((len - self.offset) as usize);
        file.read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        self.partial.extend_from_slice(&buf);

        let Some(last_newline) = self.partial.iter().rposition(|b| *b == b'\n') else {
            return Ok(Vec::new());
        };
        let rest = self.partial.split_off(last_newline + 1);
        let complete = std::mem::replace(&mut self.partial, rest);
        let mut items = Vec::new();
        for line in complete.split(|b| *b == b'\n') {
            let line = String::from_utf8_lossy(line);
            let line = line.trim();
            if !line.is_empty() {
                items.extend(self.parser.parse_line(line));
            }
        }
        Ok(items)
    }
}

/// Reads a whole JSONL file with `parser`.
pub fn read_all<P: LineParser>(path: &Path, parser: P) -> io::Result<Vec<HistoryItem>> {
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", path.display()),
        ));
    }
    let mut tail = JsonlTail::new(path, parser);
    let mut items = tail.read_new()?;
    // Accept a final line without a trailing newline.
    if !tail.partial.is_empty() {
        let line = String::from_utf8_lossy(&tail.partial).trim().to_owned();
        if !line.is_empty() {
            items.extend(tail.parser.parse_line(&line));
        }
    }
    Ok(items)
}

/// Stream of history items produced by a background tail task. Dropping the
/// receiver stops the task.
pub type HistoryStream = mpsc::Receiver<HistoryItem>;

/// Spawns a task that follows `tail` and sends new items on the returned
/// channel. Must be called within a Tokio runtime.
pub fn spawn_tail<P: LineParser>(tail: JsonlTail<P>, poll_interval: Duration) -> HistoryStream {
    let (tx, rx) = mpsc::channel(256);
    let wake = Arc::new(Notify::new());
    let watcher = make_watcher(tail.path(), Arc::clone(&wake));

    tokio::spawn(async move {
        // Keep the watcher alive for as long as the task runs.
        let _watcher = watcher;
        let mut tail = Some(tail);
        loop {
            let Some(current) = tail.take() else { break };
            let joined = tokio::task::spawn_blocking(move || {
                let mut current = current;
                let res = current.read_new();
                (current, res)
            })
            .await;
            let Ok((current, res)) = joined else { break };
            tail = Some(current);
            match res {
                Ok(items) => {
                    for item in items {
                        if tx.send(item).await.is_err() {
                            return;
                        }
                    }
                }
                Err(err) => tracing::debug!(%err, "transcript tail read failed"),
            }
            tokio::select! {
                _ = wake.notified() => {}
                _ = tokio::time::sleep(poll_interval) => {}
                _ = tx.closed() => return,
            }
        }
    });
    rx
}

fn make_watcher(path: &Path, wake: Arc<Notify>) -> Option<notify::RecommendedWatcher> {
    let dir = path.parent()?.to_path_buf();
    let file_name = path.file_name()?.to_owned();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && event
                .paths
                .iter()
                .any(|p| p.file_name() == Some(file_name.as_os_str()))
        {
            wake.notify_one();
        }
    })
    .ok()?;
    match watcher.watch(&dir, RecursiveMode::NonRecursive) {
        Ok(()) => Some(watcher),
        Err(err) => {
            tracing::debug!(%err, dir = %dir.display(), "file watch unavailable, polling only");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use vca_core::MessageRole;

    use super::*;

    fn echo_parser(line: &str) -> Vec<HistoryItem> {
        vec![HistoryItem::Message {
            at: None,
            role: MessageRole::User,
            text: line.to_owned(),
        }]
    }

    fn texts(items: &[HistoryItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                HistoryItem::Message { text, .. } => text.clone(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn keeps_partial_lines_until_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut f = File::create(&path).unwrap();
        let mut tail = JsonlTail::new(&path, echo_parser);
        write!(f, "one\ntw").unwrap();
        f.flush().unwrap();
        assert_eq!(texts(&tail.read_new().unwrap()), ["one"]);
        write!(f, "o\nthree\n").unwrap();
        f.flush().unwrap();
        assert_eq!(texts(&tail.read_new().unwrap()), ["two", "three"]);
        assert!(tail.read_new().unwrap().is_empty());
    }

    #[test]
    fn restarts_after_truncation_and_tolerates_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut tail = JsonlTail::new(&path, echo_parser);
        assert!(tail.read_new().unwrap().is_empty());
        std::fs::write(&path, "a\nb\n").unwrap();
        assert_eq!(tail.read_new().unwrap().len(), 2);
        std::fs::write(&path, "c\n").unwrap();
        assert_eq!(texts(&tail.read_new().unwrap()), ["c"]);
    }

    #[test]
    fn from_end_skips_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "old\n").unwrap();
        let mut tail = JsonlTail::from_end(&path, echo_parser).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "new").unwrap();
        assert_eq!(texts(&tail.read_new().unwrap()), ["new"]);
    }

    #[test]
    fn read_all_accepts_missing_final_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "a\nb").unwrap();
        assert_eq!(texts(&read_all(&path, echo_parser).unwrap()), ["a", "b"]);
    }

    #[tokio::test]
    async fn spawned_tail_streams_appended_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "first\n").unwrap();
        let mut rx = spawn_tail(
            JsonlTail::new(&path, echo_parser),
            Duration::from_millis(50),
        );
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(texts(&[first]), ["first"]);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "second").unwrap();
        f.flush().unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(texts(&[second]), ["second"]);
    }
}
