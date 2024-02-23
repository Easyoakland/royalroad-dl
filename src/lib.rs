#![doc=include_str!("../README.md")]

use std::{collections::VecDeque, iter::FusedIterator};

/// Buffer up to a set amount of the iterator. Useful for enabling parallelism with an iterator that spawns tasks/threads.
pub struct BufferedIter<I: Iterator> {
    iter: I,
    buffer: VecDeque<I::Item>,
}

impl<I: Iterator> BufferedIter<I> {
    /// Take up to `limit` items to fill the intermediate buffer. `0` indicates no limit.
    pub fn new(mut iter: I, limit: usize) -> Self {
        let buffer = if limit == 0 {
            VecDeque::from_iter(iter.by_ref())
        } else {
            VecDeque::from_iter(iter.by_ref().take(limit))
        };

        Self { iter, buffer }
    }
    /// Number of items currently buffered
    pub fn len(&self) -> usize {
        self.buffer.len()
    }
}
impl<I: Iterator> Iterator for BufferedIter<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let res = self.buffer.pop_front();
        if let Some(x) = self.iter.next() {
            self.buffer.push_back(x);
        }
        res
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let sh = self.iter.size_hint();
        (
            sh.0 + self.buffer.len(),
            sh.1.map(|x| x + self.buffer.len()),
        )
    }
}
impl<I: ExactSizeIterator> ExactSizeIterator for BufferedIter<I> {}
impl<I: FusedIterator> FusedIterator for BufferedIter<I> {}
