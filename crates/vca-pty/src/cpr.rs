//! Cursor position reports (CPR).
//!
//! Applications ask the terminal where the cursor is by writing a device
//! status report request, `ESC [ 6 n`, and wait for the answer
//! `ESC [ <row> ; <col> R`. Windows ConPTY sends this query at startup (the
//! pseudo console is created with the inherit-cursor flag) and blocks until
//! it is answered, so a managed session with no terminal attached would never
//! start. The session therefore always answers from its own screen state,
//! and attached terminals' answers to the same query are dropped by
//! [`CprReplyFilter`] so the application sees exactly one reply.

use std::time::{Duration, Instant};

const ESC: u8 = 0x1b;
/// Longest reply the filter buffers before giving up (`ESC[65535;65535R`).
const MAX_REPLY_LEN: usize = 16;

/// Finds `ESC [ 6 n` in a stream of output chunks, including queries split
/// across chunk boundaries.
#[derive(Debug, Default, Clone)]
pub(crate) struct CprScanner {
    /// Number of query bytes matched at the end of the previous chunk.
    matched: usize,
}

const QUERY: &[u8] = b"\x1b[6n";

impl CprScanner {
    /// Returns the offsets just past each query that ends in `data`.
    pub(crate) fn scan(&mut self, data: &[u8]) -> Vec<usize> {
        let mut ends = Vec::new();
        for (i, &b) in data.iter().enumerate() {
            if b == QUERY[self.matched] {
                self.matched += 1;
                if self.matched == QUERY.len() {
                    ends.push(i + 1);
                    self.matched = 0;
                }
            } else {
                self.matched = usize::from(b == ESC);
            }
        }
        ends
    }
}

/// The reply to a cursor position query; `row` and `col` are zero based.
pub(crate) fn cpr_reply(row: u16, col: u16) -> Vec<u8> {
    format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(col) + 1).into_bytes()
}

/// Drops a local terminal's replies to cursor position queries that the
/// session has already answered.
///
/// Call [`expect`](Self::expect) once for every query written to the local
/// terminal, then pass all input from that terminal through
/// [`filter`](Self::filter). Up to that many `ESC [ Pn ; Pn R` sequences are
/// removed. Expectations expire after a timeout so that a terminal which
/// never answers cannot cause a later key (for example Ctrl+F3, which some
/// terminals encode as `ESC[1;5R`) to be swallowed. Input is only held back
/// while a reply is expected, and only for as long as it can still turn
/// into a reply.
#[derive(Debug, Clone)]
pub struct CprReplyFilter {
    expected: usize,
    deadline: Option<Instant>,
    timeout: Duration,
    /// Bytes of a possible reply held back from the previous call.
    pending: Vec<u8>,
}

impl Default for CprReplyFilter {
    fn default() -> Self {
        Self::new(Duration::from_secs(2))
    }
}

/// How far a byte sequence matches `ESC [ digits ; digits R`.
enum Match {
    /// A valid prefix of a reply.
    Partial,
    /// A complete reply.
    Complete,
    /// Cannot become a reply.
    No,
}

fn match_reply(seq: &[u8]) -> Match {
    let mut state = 0u8;
    for (i, &b) in seq.iter().enumerate() {
        state = match (state, b) {
            (0, ESC) => 1,
            (1, b'[') => 2,
            (2 | 3, b'0'..=b'9') => 3,
            (3, b';') => 4,
            (4 | 5, b'0'..=b'9') => 5,
            (5, b'R') if i + 1 == seq.len() => return Match::Complete,
            _ => return Match::No,
        };
    }
    if seq.len() > MAX_REPLY_LEN {
        Match::No
    } else {
        Match::Partial
    }
}

impl CprReplyFilter {
    pub fn new(timeout: Duration) -> Self {
        Self {
            expected: 0,
            deadline: None,
            timeout,
            pending: Vec::new(),
        }
    }

    /// Records that `count` queries were written to the local terminal.
    pub fn expect(&mut self, count: usize) {
        if count > 0 {
            self.expected += count;
            self.deadline = Some(Instant::now() + self.timeout);
        }
    }

    /// True while replies are expected and the timeout has not passed.
    fn armed(&mut self) -> bool {
        if self.expected > 0 && self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.expected = 0;
            self.deadline = None;
        }
        self.expected > 0
    }

    /// Returns the input to forward, with expected replies removed.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() + self.pending.len());
        if !self.armed() {
            out.append(&mut self.pending);
            out.extend_from_slice(input);
            return out;
        }
        for &b in input {
            if self.expected == 0 {
                out.append(&mut self.pending);
                out.push(b);
                continue;
            }
            if self.pending.is_empty() && b != ESC {
                out.push(b);
                continue;
            }
            self.pending.push(b);
            match match_reply(&self.pending) {
                Match::Partial => {}
                Match::Complete => {
                    self.pending.clear();
                    self.expected -= 1;
                    if self.expected == 0 {
                        self.deadline = None;
                    }
                }
                Match::No => {
                    // Emit everything up to the last byte; it may start a new
                    // reply.
                    let last = self.pending.pop().unwrap_or_default();
                    out.append(&mut self.pending);
                    if last == ESC {
                        self.pending.push(last);
                    } else {
                        out.push(last);
                    }
                }
            }
        }
        out
    }

    /// Releases any held-back bytes, for example when the input ends.
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_finds_queries_in_one_chunk() {
        let mut s = CprScanner::default();
        assert_eq!(s.scan(b"ab\x1b[6ncd\x1b[6n"), vec![6, 12]);
        assert!(s.scan(b"\x1b[6m\x1b[60n").is_empty());
    }

    #[test]
    fn scanner_finds_query_split_across_chunks() {
        let mut s = CprScanner::default();
        assert!(s.scan(b"x\x1b").is_empty());
        assert!(s.scan(b"[").is_empty());
        assert_eq!(s.scan(b"6nrest"), vec![2]);
    }

    #[test]
    fn scanner_restarts_on_escape() {
        let mut s = CprScanner::default();
        assert_eq!(s.scan(b"\x1b[\x1b[6n"), vec![6]);
        assert_eq!(s.scan(b"\x1b\x1b[6n"), vec![5]);
    }

    #[test]
    fn reply_is_one_based() {
        assert_eq!(cpr_reply(0, 0), b"\x1b[1;1R");
        assert_eq!(cpr_reply(4, 79), b"\x1b[5;80R");
    }

    #[test]
    fn filter_passes_input_when_nothing_expected() {
        let mut f = CprReplyFilter::default();
        assert_eq!(f.filter(b"\x1b[3;4Rhi"), b"\x1b[3;4Rhi");
    }

    #[test]
    fn filter_drops_expected_reply() {
        let mut f = CprReplyFilter::default();
        f.expect(1);
        assert_eq!(f.filter(b"a\x1b[12;40Rb"), b"ab");
        // Only one reply was expected.
        assert_eq!(f.filter(b"\x1b[1;1R"), b"\x1b[1;1R");
    }

    #[test]
    fn filter_drops_reply_split_into_single_bytes() {
        let mut f = CprReplyFilter::default();
        f.expect(1);
        let mut out = Vec::new();
        for b in b"\x1b[2;3Rz" {
            out.extend(f.filter(&[*b]));
        }
        assert_eq!(out, b"z");
    }

    #[test]
    fn filter_releases_non_reply_sequences() {
        let mut f = CprReplyFilter::default();
        f.expect(1);
        // Arrow key, then a lone Escape followed by text.
        assert_eq!(f.filter(b"\x1b[A"), b"\x1b[A");
        assert_eq!(f.filter(b"\x1b"), b"");
        assert_eq!(f.filter(b"q"), b"\x1bq");
        assert_eq!(f.filter(b"\x1b[1;5"), b"");
        assert_eq!(f.filter(b"~"), b"\x1b[1;5~");
        // The reply is still expected.
        assert_eq!(f.filter(b"\x1b[9;9R"), b"");
    }

    #[test]
    fn filter_handles_escape_inside_partial_reply() {
        let mut f = CprReplyFilter::default();
        f.expect(1);
        assert_eq!(f.filter(b"\x1b[1\x1b[2;2R"), b"\x1b[1");
    }

    #[test]
    fn filter_expires() {
        let mut f = CprReplyFilter::new(Duration::ZERO);
        f.expect(1);
        assert_eq!(f.filter(b"\x1b[1;5R"), b"\x1b[1;5R");
    }

    #[test]
    fn flush_releases_pending() {
        let mut f = CprReplyFilter::default();
        f.expect(1);
        assert_eq!(f.filter(b"\x1b["), b"");
        assert_eq!(f.flush(), b"\x1b[");
    }
}
