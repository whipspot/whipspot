//! The ranged reader: the bytes of one stream, fetched by `Range` requests, as the
//! [`MediaSource`] the demuxer reads from.
//!
//! The fragmented MP4 demuxer reads `moov` and `sidx` at the start and then jumps to `moof`
//! offsets, so the source must make random access cheap and never fetch a block it already holds.
//! It keeps [`SLOTS`] blocks of at most [`BLOCK`] bytes in an LRU; the three buffers are allocated
//! once and reused (a fetch copies the answer into the least recently used one). A seek moves the
//! position only and does no I/O.
//!
//! Every answer is checked before a byte of it is used: the status is 206, the body is exactly the
//! range asked for (a shorter or longer one is a failure, never a short read), and a
//! `Content-Range` that is present agrees with the range and with the length. The length comes from
//! the first answer's `Content-Range` total, or yt-dlp's file size, or (for a stream smaller than a
//! block, with neither) the first body. A later total that differs fails the stream.
//!
//! A `403` means the signed URL expired: the stream is resolved again, once per track, and the
//! same range is asked for again. A second refusal fails the track.
//!
//! Errors are fixed strings ([`StreamError`]); nothing of a URL, a header or a library error is
//! kept.

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use symphonia::core::io::MediaSource;

use crate::http::{Cap, ContentRange, Http};
use crate::resolve::{RangeForm, Stream};

/// The block size: one ranged request fetches at most this many bytes.
pub const BLOCK: u64 = 512 * 1024;
/// The smallest block used, however small yt-dlp's own chunk size is.
pub const MIN_BLOCK: u64 = 16 * 1024;
/// How many blocks are held.
pub const SLOTS: usize = 3;

/// Why a stream could not be read. Fixed messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    /// The server refused the link, again after it was resolved anew.
    Refused,
    /// The answer was not what was asked for: a status, a length or a `Content-Range` that does
    /// not fit.
    Bad,
    /// The connection failed.
    Network,
    /// The track was stopped or replaced while a read was due.
    Cancelled,
    /// The stream asks for a way of reading that is not implemented.
    Unsupported,
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StreamError::Refused => "the stream link was refused",
            StreamError::Bad => "the stream's answer was not usable",
            StreamError::Network => "the stream could not be reached",
            StreamError::Cancelled => "the stream was closed",
            StreamError::Unsupported => "the stream cannot be read this way",
        })
    }
}

impl std::error::Error for StreamError {}

impl StreamError {
    fn io(self) -> io::Error {
        let kind = match self {
            StreamError::Cancelled => io::ErrorKind::ConnectionAborted,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, self)
    }
}

/// Whether the reader still serves the track it was made for. One counter per player: starting
/// another track, a stop or a shutdown raises it.
#[derive(Clone)]
pub struct Cancel {
    counter: Arc<AtomicU64>,
    generation: u64,
}

impl Cancel {
    pub fn new(counter: Arc<AtomicU64>) -> Cancel {
        let generation = counter.load(Ordering::Acquire);
        Cancel {
            counter,
            generation,
        }
    }

    /// A reader that is never cancelled.
    pub fn never() -> Cancel {
        Cancel::new(Arc::new(AtomicU64::new(0)))
    }

    pub fn is_cancelled(&self) -> bool {
        self.counter.load(Ordering::Acquire) != self.generation
    }
}

/// Resolves the stream again (the URL expired). `None`: it could not be.
pub type Refresh = Box<dyn FnMut() -> Option<Stream> + Send>;

struct Slot {
    /// The block this slot holds, if any.
    index: Option<u64>,
    data: Vec<u8>,
    /// For the LRU: the tick of the last use.
    used: u64,
}

/// A stream read by ranged GETs. See the module note.
pub struct RangeSource {
    http: Arc<dyn Http>,
    stream: Stream,
    refresh: Mutex<Refresh>,
    refreshed: bool,
    cancel: Cancel,
    /// The size of the stream.
    len: u64,
    block: u64,
    pos: u64,
    slots: Vec<Slot>,
    tick: u64,
}

fn unpoison<T>(r: Result<T, std::sync::PoisonError<T>>) -> T {
    r.unwrap_or_else(PoisonError::into_inner)
}

/// The block size for a stream: [`BLOCK`], or yt-dlp's own chunk size when smaller (but never
/// under [`MIN_BLOCK`]).
pub fn block_size(stream: &Stream) -> u64 {
    stream
        .chunk_size
        .map_or(BLOCK, |c| c.min(BLOCK))
        .max(MIN_BLOCK)
}

impl RangeSource {
    /// Opens the stream: fetches its first block, which also settles the length.
    pub fn open(
        http: Arc<dyn Http>,
        stream: Stream,
        refresh: Refresh,
        cancel: Cancel,
    ) -> Result<RangeSource, StreamError> {
        if stream.range_form != RangeForm::Header {
            return Err(StreamError::Unsupported);
        }
        let block = block_size(&stream);
        let mut source = RangeSource {
            http,
            len: stream.filesize.unwrap_or(0),
            stream,
            refresh: Mutex::new(refresh),
            refreshed: false,
            cancel,
            block,
            pos: 0,
            slots: Vec::new(),
            tick: 0,
        };
        source.fetch_first()?;
        Ok(source)
    }

    /// The size of the stream in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The capacity of each held buffer (for the test that the buffers are reused).
    pub fn buffer_capacities(&self) -> Vec<usize> {
        self.slots.iter().map(|s| s.data.capacity()).collect()
    }

    /// The first block. The length may be unknown until its answer is read.
    fn fetch_first(&mut self) -> Result<(), StreamError> {
        let known = self.len > 0;
        let end = if known {
            self.len.min(self.block)
        } else {
            self.block
        } - 1;
        let (body, cr) = self.get(0, end)?;
        let total = cr.and_then(|c| c.total);
        let len = match (total, known) {
            // The server's own total wins over yt-dlp's figure.
            (Some(t), _) => t,
            (None, true) => self.len,
            // A stream shorter than one block, with no total: the body is all of it.
            (None, false) if (body.len() as u64) < self.block => body.len() as u64,
            (None, false) => return Err(StreamError::Bad),
        };
        if len == 0 || body.len() as u64 != len.min(self.block) {
            return Err(StreamError::Bad);
        }
        self.len = len;
        self.store(0, &body);
        Ok(())
    }

    /// One ranged GET with the one re-resolve on a `403`, and the checks of the module note.
    /// Returns the body and the `Content-Range` that came with it.
    fn get(&mut self, a: u64, b: u64) -> Result<(Vec<u8>, Option<ContentRange>), StreamError> {
        match self.request(a, b) {
            Err(StreamError::Refused) if !self.refreshed => {
                self.refreshed = true;
                if self.cancel.is_cancelled() {
                    return Err(StreamError::Cancelled);
                }
                let fresh = (unpoison(self.refresh.lock()))().ok_or(StreamError::Refused)?;
                // Another file, or another way of asking, is not the same stream.
                let size_changed = self.len > 0 && fresh.filesize.is_some_and(|f| f != self.len);
                if fresh.range_form != RangeForm::Header || size_changed {
                    return Err(StreamError::Refused);
                }
                self.stream = fresh;
                self.request(a, b)
            }
            other => other,
        }
    }

    fn request(&self, a: u64, b: u64) -> Result<(Vec<u8>, Option<ContentRange>), StreamError> {
        if self.cancel.is_cancelled() {
            return Err(StreamError::Cancelled);
        }
        let headers: Vec<(&str, &str)> = self
            .stream
            .headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let answer = self
            .http
            .get_range(&self.stream.url, &headers, (a, b), Cap::MEDIA)
            .map_err(|_| StreamError::Network)?;
        match answer.status {
            206 => {}
            403 => return Err(StreamError::Refused),
            _ => return Err(StreamError::Bad),
        }
        let n = answer.body.len() as u64;
        if n == 0 || n > b - a + 1 {
            return Err(StreamError::Bad);
        }
        if let Some(cr) = answer.content_range {
            if cr.start != a || cr.end != a + n - 1 {
                return Err(StreamError::Bad);
            }
            // The total never changes: the first one read is the one kept.
            if cr
                .total
                .is_some_and(|t| self.len > 0 && t != self.len && self.slots_filled())
            {
                return Err(StreamError::Bad);
            }
        }
        Ok((answer.body, answer.content_range))
    }

    fn slots_filled(&self) -> bool {
        !self.slots.is_empty()
    }

    /// Copies `body` into the least recently used slot as block `index`.
    fn store(&mut self, index: u64, body: &[u8]) -> usize {
        self.tick += 1;
        let at = if self.slots.len() < SLOTS {
            self.slots.push(Slot {
                index: None,
                data: Vec::with_capacity(self.block as usize),
                used: 0,
            });
            self.slots.len() - 1
        } else {
            self.slots
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.used)
                .map_or(0, |(i, _)| i)
        };
        let slot = &mut self.slots[at];
        slot.data.clear();
        slot.data.extend_from_slice(body);
        slot.index = Some(index);
        slot.used = self.tick;
        at
    }

    /// The slot holding block `index`, fetching it if it is not held.
    fn slot_of(&mut self, index: u64) -> Result<usize, StreamError> {
        if let Some(at) = self.slots.iter().position(|s| s.index == Some(index)) {
            self.tick += 1;
            self.slots[at].used = self.tick;
            return Ok(at);
        }
        let a = index * self.block;
        let b = (a + self.block).min(self.len) - 1;
        let (body, _) = self.get(a, b)?;
        if body.len() as u64 != b - a + 1 {
            return Err(StreamError::Bad);
        }
        Ok(self.store(index, &body))
    }
}

impl Read for RangeSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let index = self.pos / self.block;
        let at = self.slot_of(index).map_err(StreamError::io)?;
        let slot = &self.slots[at];
        let offset = usize::try_from(self.pos - index * self.block).unwrap_or(usize::MAX);
        let n = out.len().min(slot.data.len().saturating_sub(offset));
        out[..n].copy_from_slice(&slot.data[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RangeSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = i128::from(self.len);
        let target = match to {
            SeekFrom::Start(p) => i128::from(p),
            SeekFrom::End(d) => len + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start",
            ));
        }
        self.pos = u64::try_from(target).unwrap_or(u64::MAX).min(self.len);
        Ok(self.pos)
    }
}

impl MediaSource for RangeSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}
