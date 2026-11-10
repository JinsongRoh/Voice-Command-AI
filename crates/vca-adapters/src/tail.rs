//! Incremental reading of JSONL transcript files.
//!
//! [`JsonlTail`] remembers how far a file has been read and turns newly
//! appended complete lines into [`HistoryItem`]s. [`spawn_tail`] drives it
//! from a background task that wakes up on file system notifications and,
//! as a fallback, on a fixed polling interval.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
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
    /// Hashes of the lines reported last, so that a file that was rewritten
    /// in place (a compaction, a re-login) does not report its unchanged
    /// lines a second time. Bounded: a session that runs for days must not
    /// keep a hash of every line it ever wrote.
    seen: SeenLines,
    /// The file shrank and is being read again from the start.
    rereading: bool,
    /// The file shrank since the last report: the next one starts with
    /// [`HistoryItem::Rewritten`].
    rewritten: bool,
    /// Bytes of most [`READ_CHUNK`] per read (tests make it small).
    chunk: usize,
    /// More of the file is waiting than the last read took.
    more: bool,
    /// A line grew past [`MAX_LINE_BYTES`]: its rest is skipped up to the
    /// next newline.
    skipping: bool,
}

/// Most bytes one read takes from the file: a long transcript is read in
/// steps, so it never sits in memory whole.
pub const READ_CHUNK: usize = 1 << 20;
/// Longest line kept. A longer one (an incomplete write that never ends,
/// or a corrupt file) is dropped instead of growing without bound.
pub const MAX_LINE_BYTES: usize = 16 << 20;

/// Most line hashes a tail remembers (about 0.5 MB at most). A rewrite
/// keeps the recent part of a transcript; lines older than this that
/// reappear after one are reported again, which is harmless.
const SEEN_LINES: usize = 20_000;

/// A set of line hashes that forgets the oldest beyond its capacity.
#[derive(Debug)]
struct SeenLines {
    set: HashSet<u64>,
    order: VecDeque<u64>,
    cap: usize,
}

impl SeenLines {
    fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Adds a hash; true when it was not there.
    fn insert(&mut self, hash: u64) -> bool {
        if !self.set.insert(hash) {
            return false;
        }
        self.order.push_back(hash);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    fn len(&self) -> usize {
        self.set.len()
    }
}

fn line_hash(line: &str) -> u64 {
    let mut h = DefaultHasher::new();
    line.hash(&mut h);
    h.finish()
}

impl<P: LineParser> JsonlTail<P> {
    /// Starts reading at the beginning of the file.
    pub fn new(path: impl Into<PathBuf>, parser: P) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            partial: Vec::new(),
            parser,
            seen: SeenLines::new(SEEN_LINES),
            rereading: false,
            rewritten: false,
            chunk: READ_CHUNK,
            more: false,
            skipping: false,
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
            seen: SeenLines::new(SEEN_LINES),
            rereading: false,
            rewritten: false,
            chunk: READ_CHUNK,
            more: false,
            skipping: false,
        })
    }

    /// Whether the last read stopped at [`READ_CHUNK`] with more of the file
    /// waiting: read again right away.
    pub fn has_more(&self) -> bool {
        self.more
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads everything appended since the last call. A missing file yields
    /// nothing; a truncated file is read again from the start, skipping lines
    /// that were reported before, and the report starts with
    /// [`HistoryItem::Rewritten`]. An incomplete last line is kept until its
    /// newline arrives.
    pub fn read_new(&mut self) -> io::Result<Vec<HistoryItem>> {
        let mut items = self.read_lines()?;
        if std::mem::take(&mut self.rewritten) {
            items.insert(0, HistoryItem::Rewritten);
        }
        Ok(items)
    }

    fn read_lines(&mut self) -> io::Result<Vec<HistoryItem>> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let len = file.metadata()?.len();
        self.more = false;
        if len < self.offset {
            self.offset = 0;
            self.partial = Vec::new();
            self.skipping = false;
            self.rereading = true;
            self.rewritten = true;
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let want = usize::try_from(len - self.offset)
            .unwrap_or(usize::MAX)
            .min(self.chunk.max(1));
        let mut buf = Vec::with_capacity(want);
        (&mut file).take(want as u64).read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        self.more = !buf.is_empty() && self.offset < len;
        let mut start = 0;
        if self.skipping {
            match buf.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    self.skipping = false;
                    start = i + 1;
                }
                None => return Ok(Vec::new()),
            }
        }
        self.partial.extend_from_slice(&buf[start..]);
        drop(buf);

        let Some(last_newline) = self.partial.iter().rposition(|b| *b == b'\n') else {
            self.limit_partial();
            return Ok(Vec::new());
        };
        let rest = self.partial.split_off(last_newline + 1);
        let complete = std::mem::replace(&mut self.partial, rest);
        self.limit_partial();
        let mut items = Vec::new();
        for line in complete.split(|b| *b == b'\n') {
            let line = String::from_utf8_lossy(line);
            let line = line.trim();
            if !line.is_empty() && self.accept(line) {
                items.extend(self.parser.parse_line(line));
            }
        }
        Ok(items)
    }
}

impl<P> JsonlTail<P> {
    /// Drops an incomplete line that grew past [`MAX_LINE_BYTES`] and skips
    /// the rest of it.
    fn limit_partial(&mut self) {
        if self.partial.len() > MAX_LINE_BYTES {
            tracing::warn!(
                path = %self.path.display(),
                bytes = self.partial.len(),
                "transcript line too long, skipped"
            );
            self.partial = Vec::new();
            self.skipping = true;
        }
    }

    /// Line hashes held for rewrite detection.
    pub fn remembered_lines(&self) -> usize {
        self.seen.len()
    }

    /// Whether a line is to be reported. Before the file has ever shrunk
    /// every line is (identical lines can be legitimate); afterwards only
    /// lines not reported before are.
    fn accept(&mut self, line: &str) -> bool {
        let is_new = self.seen.insert(line_hash(line));
        is_new || !self.rereading
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
    while tail.has_more() {
        items.extend(tail.read_new()?);
    }
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
            if tail.as_ref().is_some_and(JsonlTail::has_more) {
                // The rest of a long file: no need to wait for a change.
                continue;
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
        let items = tail.read_new().unwrap();
        assert_eq!(items[0], HistoryItem::Rewritten);
        assert_eq!(texts(&items[1..]), ["c"]);
    }

    #[test]
    fn rewritten_file_reports_only_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut tail = JsonlTail::new(&path, echo_parser);
        std::fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").unwrap();
        assert_eq!(tail.read_new().unwrap().len(), 4);
        // A compaction rewrites the file: shorter, with a summary line.
        std::fs::write(&path, "summary\ngamma\ndelta\n").unwrap();
        assert_eq!(
            tail.read_new().unwrap(),
            [HistoryItem::Rewritten, echo_parser("summary").remove(0)]
        );
        // Appends after the rewrite keep flowing.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "epsilon").unwrap();
        assert_eq!(texts(&tail.read_new().unwrap()), ["epsilon"]);
    }

    #[test]
    fn remembered_lines_are_bounded() {
        let mut seen = SeenLines::new(3);
        for h in 0..10 {
            assert!(seen.insert(h));
        }
        assert_eq!(seen.len(), 3);
        // The newest are kept, the oldest forgotten.
        assert!(!seen.insert(9));
        assert!(seen.insert(0));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let lines: String = (0..SEEN_LINES + 500)
            .map(|i| format!("line {i}\n"))
            .collect();
        std::fs::write(&path, lines).unwrap();
        let mut tail = JsonlTail::new(&path, echo_parser);
        assert_eq!(tail.read_new().unwrap().len(), SEEN_LINES + 500);
        assert_eq!(tail.remembered_lines(), SEEN_LINES);
        // A rewrite that keeps the recent lines still reports only new ones.
        let kept: String = (SEEN_LINES..SEEN_LINES + 500)
            .map(|i| format!("line {i}\n"))
            .collect();
        std::fs::write(&path, format!("summary\n{kept}")).unwrap();
        assert_eq!(
            tail.read_new().unwrap(),
            [HistoryItem::Rewritten, echo_parser("summary").remove(0)]
        );
    }

    #[test]
    fn identical_lines_are_kept_when_the_file_never_shrank() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "same\nsame\n").unwrap();
        let mut tail = JsonlTail::new(&path, echo_parser);
        assert_eq!(texts(&tail.read_new().unwrap()), ["same", "same"]);
    }

    #[test]
    fn long_files_are_read_in_chunks_and_huge_lines_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "one").unwrap();
        writeln!(f, "two").unwrap();
        writeln!(f, "three").unwrap();
        f.flush().unwrap();
        let mut tail = JsonlTail::new(&path, echo_parser);
        tail.chunk = 5;
        let mut got = Vec::new();
        loop {
            got.extend(tail.read_new().unwrap());
            if !tail.has_more() {
                break;
            }
        }
        assert_eq!(texts(&got), ["one", "two", "three"]);
        assert!(tail.partial.capacity() < 64);

        // A line past the cap is dropped; the lines after it still arrive.
        let mut tail = JsonlTail::new(&path, echo_parser);
        tail.read_new().unwrap();
        tail.partial = vec![b'x'; MAX_LINE_BYTES + 1];
        tail.limit_partial();
        assert!(tail.partial.is_empty() && tail.skipping);
        write!(f, "rest of the long line\nfour\n").unwrap();
        f.flush().unwrap();
        assert_eq!(texts(&tail.read_new().unwrap()), ["four"]);
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
