//! Byte spans: the lingua franca of every compiler stage.

use std::ops::Range;

/// A half-open byte range `[start, end)` into a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "Span start must be <= end");
        Self { start, end }
    }

    /// Zero-width span (a cursor position).
    pub fn empty(at: usize) -> Self {
        Self { start: at, end: at }
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Smallest span covering both `self` and `other`.
    pub fn merge(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

impl From<Span> for Range<usize> {
    fn from(s: Span) -> Self {
        s.range()
    }
}
