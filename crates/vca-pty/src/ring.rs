//! Bounded ring buffer of output chunks tagged with sequence numbers.

use std::collections::VecDeque;

use bytes::Bytes;

/// A piece of terminal output. Sequence numbers start at 0 and increase by
/// one per chunk for the lifetime of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputChunk {
    pub seq: u64,
    pub data: Bytes,
}

/// Output that a reader asked for but that was already evicted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// First sequence number the reader wanted.
    pub requested: u64,
    /// Oldest sequence number still available; the replay resumes here.
    pub resumed_at: u64,
}

impl Gap {
    /// Number of chunks that were lost.
    pub fn missed(&self) -> u64 {
        self.resumed_at.saturating_sub(self.requested)
    }
}

/// Result of [`OutputRing::replay_from`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Replay {
    /// Set when part of the requested range was evicted.
    pub gap: Option<Gap>,
    /// Retained chunks from the requested sequence (or the oldest retained).
    pub chunks: Vec<OutputChunk>,
}

/// Limits of the output ring buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferLimits {
    /// Maximum total payload bytes retained.
    pub max_bytes: usize,
    /// Maximum number of chunks retained.
    pub max_chunks: usize,
}

impl Default for BufferLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024,
            max_chunks: 4096,
        }
    }
}

/// Ring buffer of the most recent output chunks.
#[derive(Debug)]
pub struct OutputRing {
    chunks: VecDeque<OutputChunk>,
    bytes: usize,
    limits: BufferLimits,
    next_seq: u64,
}

impl OutputRing {
    pub fn new(limits: BufferLimits) -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            limits: BufferLimits {
                max_bytes: limits.max_bytes.max(1),
                max_chunks: limits.max_chunks.max(1),
            },
            next_seq: 0,
        }
    }

    /// Sequence number the next pushed chunk will get.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Oldest retained sequence number (equals [`next_seq`](Self::next_seq)
    /// when the buffer is empty).
    pub fn oldest_seq(&self) -> u64 {
        self.chunks.front().map_or(self.next_seq, |c| c.seq)
    }

    /// Appends a chunk and evicts old ones beyond the limits. The newest
    /// chunk is always retained, even if it alone exceeds `max_bytes`.
    pub fn push(&mut self, data: Bytes) -> OutputChunk {
        let chunk = OutputChunk {
            seq: self.next_seq,
            data,
        };
        self.next_seq += 1;
        self.bytes += chunk.data.len();
        self.chunks.push_back(chunk.clone());
        while self.chunks.len() > 1
            && (self.bytes > self.limits.max_bytes || self.chunks.len() > self.limits.max_chunks)
        {
            if let Some(old) = self.chunks.pop_front() {
                self.bytes -= old.data.len();
            }
        }
        chunk
    }

    /// Returns retained chunks with `seq >= from`, and a [`Gap`] when some
    /// of them were already evicted.
    pub fn replay_from(&self, from: u64) -> Replay {
        let oldest = self.oldest_seq();
        let gap = (from < oldest).then_some(Gap {
            requested: from,
            resumed_at: oldest,
        });
        let start = from.max(oldest);
        let skip = usize::try_from(start - oldest).unwrap_or(usize::MAX);
        Replay {
            gap,
            chunks: self.chunks.iter().skip(skip).cloned().collect(),
        }
    }

    /// Total payload bytes currently retained.
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(max_bytes: usize, max_chunks: usize) -> OutputRing {
        OutputRing::new(BufferLimits {
            max_bytes,
            max_chunks,
        })
    }

    #[test]
    fn sequence_numbers_increase() {
        let mut r = ring(100, 10);
        assert_eq!(r.push(Bytes::from_static(b"a")).seq, 0);
        assert_eq!(r.push(Bytes::from_static(b"b")).seq, 1);
        assert_eq!(r.next_seq(), 2);
    }

    #[test]
    fn evicts_by_chunk_count_and_reports_gap() {
        let mut r = ring(100, 2);
        for b in [b"a", b"b", b"c"] {
            r.push(Bytes::from_static(b));
        }
        assert_eq!(r.oldest_seq(), 1);
        let replay = r.replay_from(0);
        assert_eq!(
            replay.gap,
            Some(Gap {
                requested: 0,
                resumed_at: 1
            })
        );
        assert_eq!(replay.gap.unwrap().missed(), 1);
        let seqs: Vec<_> = replay.chunks.iter().map(|c| c.seq).collect();
        assert_eq!(seqs, vec![1, 2]);
    }

    #[test]
    fn evicts_by_bytes_but_keeps_newest() {
        let mut r = ring(4, 100);
        r.push(Bytes::from_static(b"abc"));
        r.push(Bytes::from_static(b"de"));
        assert_eq!(r.oldest_seq(), 1);
        assert_eq!(r.retained_bytes(), 2);
        r.push(Bytes::from_static(b"0123456789"));
        assert_eq!(r.oldest_seq(), 2);
        assert_eq!(r.retained_bytes(), 10);
    }

    #[test]
    fn replay_from_middle_and_future() {
        let mut r = ring(100, 10);
        for b in [b"a", b"b", b"c"] {
            r.push(Bytes::from_static(b));
        }
        let replay = r.replay_from(1);
        assert!(replay.gap.is_none());
        assert_eq!(replay.chunks.len(), 2);
        assert_eq!(replay.chunks[0].data, Bytes::from_static(b"b"));
        assert!(r.replay_from(3).chunks.is_empty());
        assert!(r.replay_from(42).gap.is_none());
    }

    #[test]
    fn empty_ring_has_no_gap() {
        let r = ring(10, 10);
        let replay = r.replay_from(0);
        assert!(replay.gap.is_none());
        assert!(replay.chunks.is_empty());
    }
}
