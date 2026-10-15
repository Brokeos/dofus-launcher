//! Bundle downloads: multi-range request planning, `multipart/byteranges` streaming parser,
//! per-chunk collection + SHA-1 verification, retries with backoff, optional speed limit.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use sha1::Digest;
use tokio_util::sync::CancellationToken;

use super::CytrusError;
use super::manifest::Hash;

/// Max length of the `Range` header value (same limit as the official client).
pub(crate) const MAX_RANGE_HEADER: usize = 6144;
/// Max payload bytes per request: bounds memory, retry cost and keeps the work spread over connections.
pub(crate) const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
/// A response that stays silent this long is aborted and retried.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: u32 = 6;

/// A chunk to fetch from a bundle blob (offset = position inside the blob).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WantedChunk {
    pub hash: Hash,
    pub offset: u64,
    pub size: u64,
}

/// One HTTP request worth of chunks of a single bundle, sorted by offset.
#[derive(Debug, Clone)]
pub(crate) struct RangeRequest {
    pub bundle: Hash,
    pub chunks: Vec<WantedChunk>,
}

impl RangeRequest {
    pub fn bytes(&self) -> u64 {
        self.chunks.iter().map(|c| c.size).sum()
    }
}

/// Merge chunks (sorted by offset) into inclusive byte ranges; adjacent or overlapping chunks coalesce.
pub(crate) fn merge_ranges(chunks: &[WantedChunk]) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for c in chunks.iter().filter(|c| c.size > 0) {
        let end = c.offset + c.size - 1;
        match out.last_mut() {
            Some(last) if c.offset <= last.1 + 1 => last.1 = last.1.max(end),
            _ => out.push((c.offset, end)),
        }
    }
    out
}

pub(crate) fn range_header(ranges: &[(u64, u64)]) -> String {
    let mut s = String::from("bytes=");
    for (i, (a, b)) in ranges.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{a}-{b}"));
    }
    s
}

/// Split the wanted chunks of one bundle into requests whose `Range` header fits `max_header`
/// characters and whose payload stays under `max_bytes` (a single larger chunk gets its own request).
pub(crate) fn plan_requests(bundle: Hash, mut chunks: Vec<WantedChunk>, max_header: usize, max_bytes: u64) -> Vec<RangeRequest> {
    chunks.sort_by_key(|c| c.offset);
    chunks.dedup_by_key(|c| c.offset);
    let mut out = Vec::new();
    let mut cur: Vec<WantedChunk> = Vec::new();
    let mut cur_bytes = 0u64;
    let mut header_len = "bytes=".len();
    let mut last_end: Option<u64> = None; // exclusive end of the last range
    for c in chunks {
        let contiguous = last_end == Some(c.offset);
        // Cost of this chunk in header characters: extending a range rewrites its end only.
        let added = if contiguous {
            let prev_end = last_end.unwrap() - 1;
            digits(c.offset + c.size - 1) as isize - digits(prev_end) as isize
        } else {
            (usize::from(!cur.is_empty()) + digits(c.offset) + 1 + digits(c.offset + c.size - 1)) as isize
        };
        let too_long = header_len as isize + added > max_header as isize;
        if !cur.is_empty() && (too_long || cur_bytes + c.size > max_bytes) {
            out.push(RangeRequest { bundle, chunks: std::mem::take(&mut cur) });
            cur_bytes = 0;
            header_len = "bytes=".len();
        }
        if cur.is_empty() {
            header_len += digits(c.offset) + 1 + digits(c.offset + c.size - 1);
        } else {
            header_len = (header_len as isize + added) as usize;
        }
        cur_bytes += c.size;
        last_end = Some(c.offset + c.size);
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(RangeRequest { bundle, chunks: cur });
    }
    out
}

fn digits(mut n: u64) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Parse `bytes a-b/total` (or `bytes a-b/*`) into (a, b).
pub(crate) fn parse_content_range(v: &str) -> Option<(u64, u64)> {
    let v = v.trim();
    let rest = v.strip_prefix("bytes").unwrap_or(v).trim_start();
    let range = rest.split('/').next()?.trim();
    let (a, b) = range.split_once('-')?;
    let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (b >= a).then_some((a, b))
}

/// Extract the boundary from `multipart/byteranges; boundary=XYZ`.
pub(crate) fn multipart_boundary(content_type: &str) -> Option<String> {
    let (mime, params) = content_type.split_once(';')?;
    if !mime.trim().eq_ignore_ascii_case("multipart/byteranges") {
        return None;
    }
    params.split(';').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("boundary").then(|| v.trim().trim_matches('"').to_string())
    })
}

#[derive(Debug, PartialEq, Eq)]
enum MpState {
    Preamble,
    Body,
    Done,
}

/// Incremental `multipart/byteranges` parser. Body bytes are passed to the sink without copying;
/// only delimiter/header bytes are buffered.
pub(crate) struct MultipartParser {
    delim: Vec<u8>,
    state: MpState,
    buf: Vec<u8>,
    offset: u64,
    remaining: u64,
}

impl MultipartParser {
    pub fn new(boundary: &str) -> Self {
        MultipartParser { delim: format!("--{boundary}").into_bytes(), state: MpState::Preamble, buf: Vec::new(), offset: 0, remaining: 0 }
    }

    #[cfg(test)]
    pub fn is_done(&self) -> bool {
        self.state == MpState::Done
    }

    /// Feed response bytes; `sink(abs_offset, data)` receives body segments in order.
    pub fn feed(&mut self, mut input: &[u8], sink: &mut dyn FnMut(u64, &[u8])) -> Result<()> {
        loop {
            match self.state {
                MpState::Done => return Ok(()),
                MpState::Body => {
                    if input.is_empty() {
                        return Ok(());
                    }
                    let n = (self.remaining.min(input.len() as u64)) as usize;
                    sink(self.offset, &input[..n]);
                    self.offset += n as u64;
                    self.remaining -= n as u64;
                    input = &input[n..];
                    if self.remaining == 0 {
                        self.state = MpState::Preamble;
                    }
                }
                MpState::Preamble => {
                    let before = self.buf.len();
                    self.buf.extend_from_slice(input);
                    match self.parse_head()? {
                        Some(body_start) => {
                            let skip = body_start.saturating_sub(before).min(input.len());
                            debug_assert!(body_start >= before);
                            self.buf.clear();
                            input = &input[skip..];
                            self.state = if self.remaining == 0 { MpState::Preamble } else { MpState::Body };
                        }
                        None => {
                            if self.state == MpState::Preamble && find(&self.buf, &self.delim).is_none() {
                                let keep = self.delim.len() + 2;
                                if self.buf.len() > keep {
                                    self.buf.drain(..self.buf.len() - keep);
                                }
                            } else if self.buf.len() > 64 * 1024 {
                                bail!("multipart: part headers too large");
                            }
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    /// Try to parse `--boundary[--]\r\n headers \r\n\r\n` from `buf`. Returns the body start index.
    fn parse_head(&mut self) -> Result<Option<usize>> {
        let Some(i) = find(&self.buf, &self.delim) else { return Ok(None) };
        let after = i + self.delim.len();
        if self.buf.len() < after + 2 {
            return Ok(None);
        }
        if &self.buf[after..after + 2] == b"--" {
            self.state = MpState::Done;
            return Ok(None);
        }
        // Rest of the delimiter line (transport padding), then header lines until an empty line.
        let Some(nl) = self.buf[after..].iter().position(|&c| c == b'\n') else { return Ok(None) };
        let mut pos = after + nl + 1;
        let mut range = None;
        loop {
            let Some(nl) = self.buf[pos..].iter().position(|&c| c == b'\n') else { return Ok(None) };
            let line = &self.buf[pos..pos + nl];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            pos += nl + 1;
            if line.is_empty() {
                break;
            }
            let line = String::from_utf8_lossy(line);
            if let Some((k, v)) = line.split_once(':')
                && k.trim().eq_ignore_ascii_case("content-range") {
                    range = Some(parse_content_range(v).with_context(|| format!("multipart: bad Content-Range {v:?}"))?);
                }
        }
        let (a, b) = range.context("multipart: part without Content-Range")?;
        self.offset = a;
        self.remaining = b - a + 1;
        Ok(Some(pos))
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Collects body segments into whole chunks (sorted by blob offset).
pub(crate) struct ChunkCollector {
    chunks: Vec<WantedChunk>,
    bufs: Vec<Vec<u8>>,
    done: Vec<bool>,
    left: usize,
}

impl ChunkCollector {
    pub fn new(mut chunks: Vec<WantedChunk>) -> Self {
        chunks.sort_by_key(|c| c.offset);
        let n = chunks.len();
        ChunkCollector { chunks, bufs: vec![Vec::new(); n], done: vec![false; n], left: n }
    }

    pub fn all_done(&self) -> bool {
        self.left == 0
    }

    /// Chunks that were not completed yet.
    pub fn remaining(&self) -> Vec<WantedChunk> {
        self.chunks.iter().zip(&self.done).filter(|(_, d)| !**d).map(|(c, _)| *c).collect()
    }

    /// Feed `data` located at blob offset `abs`; completed chunks are pushed to `out` (index, bytes).
    pub fn feed(&mut self, abs: u64, data: &[u8], out: &mut Vec<(WantedChunk, Vec<u8>)>) {
        let end = abs + data.len() as u64;
        let start_idx = self.chunks.partition_point(|c| c.offset + c.size <= abs);
        for i in start_idx..self.chunks.len() {
            let c = self.chunks[i];
            if c.offset >= end {
                break;
            }
            if self.done[i] {
                continue;
            }
            let buf = &mut self.bufs[i];
            let expected = c.offset + buf.len() as u64; // next byte this chunk needs
            let ov_end = end.min(c.offset + c.size);
            if expected < abs || expected >= ov_end {
                continue; // gap (out-of-order data) or nothing new: the chunk will be retried
            }
            if buf.capacity() == 0 {
                buf.reserve_exact(c.size as usize);
            }
            buf.extend_from_slice(&data[(expected - abs) as usize..(ov_end - abs) as usize]);
            if buf.len() as u64 == c.size {
                self.done[i] = true;
                self.left -= 1;
                out.push((c, std::mem::take(buf)));
            }
        }
    }
}

/// Byte-rate limiter shared by all requests (virtual clock, 0.5 s burst allowance).
pub(crate) struct RateLimiter {
    bps: u64,
    next: std::sync::Mutex<Instant>,
}

impl RateLimiter {
    pub fn new(bps: u64) -> Self {
        RateLimiter { bps: bps.max(1024), next: std::sync::Mutex::new(Instant::now()) }
    }
    pub async fn consume(&self, n: usize) {
        let wake = {
            let mut next = self.next.lock().unwrap();
            let now = Instant::now();
            if *next < now {
                *next = now;
            }
            *next += Duration::from_secs_f64(n as f64 / self.bps as f64);
            next.checked_sub(Duration::from_millis(500)).filter(|w| *w > now)
        };
        if let Some(w) = wake {
            tokio::time::sleep_until(w.into()).await;
        }
    }
}

/// Receives verified chunk bytes (called on a blocking thread).
pub(crate) trait ChunkSink: Send + Sync + 'static {
    fn write(&self, hash: &Hash, data: &[u8]) -> Result<()>;
}

pub(crate) struct FetchCtx {
    pub client: reqwest::Client,
    pub cancel: CancellationToken,
    pub limiter: Option<Arc<RateLimiter>>,
    pub sink: Arc<dyn ChunkSink>,
    /// Raw bytes received (for speed).
    pub received: Arc<AtomicU64>,
    /// Verified chunk bytes delivered.
    pub delivered: Arc<AtomicU64>,
    /// Cleared when the server answers a multi-range request with a full 200: fall back to single ranges.
    pub multi_range_ok: Arc<AtomicBool>,
    pub url_for: fn(&Hash) -> String,
}

enum FetchErr {
    Retry(anyhow::Error),
    Fatal(anyhow::Error),
}

/// Fetch all chunks of a request, retrying with exponential backoff on network errors, 5xx,
/// 416/429, truncated bodies and chunk hash mismatches. Chunks completed by a failed attempt are kept.
pub(crate) async fn fetch_request(ctx: &FetchCtx, req: RangeRequest) -> Result<()> {
    let url = (ctx.url_for)(&req.bundle);
    let mut remaining = req.chunks;
    let mut attempt = 0u32;
    let mut delay = Duration::from_millis(500);
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(CytrusError::Cancelled.into());
        }
        let before = remaining.len();
        let ranges = merge_ranges(&remaining);
        let res = if ranges.len() > 1 && !ctx.multi_range_ok.load(Ordering::Relaxed) {
            // Server ignores multi-range: one range at a time.
            let mut r = Ok(());
            for range in ranges {
                let sub: Vec<_> = remaining.iter().copied().filter(|c| c.offset >= range.0 && c.offset <= range.1).collect();
                let mut left = sub.clone();
                r = fetch_once(ctx, &url, &mut left).await;
                remaining.retain(|c| !sub.contains(c) || left.contains(c));
                if r.is_err() {
                    break;
                }
            }
            r
        } else {
            fetch_once(ctx, &url, &mut remaining).await
        };
        match res {
            Ok(()) if remaining.is_empty() => return Ok(()),
            Ok(()) => {}
            Err(FetchErr::Fatal(e)) => return Err(e),
            Err(FetchErr::Retry(e)) => {
                if ctx.cancel.is_cancelled() {
                    return Err(CytrusError::Cancelled.into());
                }
                if remaining.len() < before {
                    attempt = 0; // progress was made: the connection is just flaky
                    delay = Duration::from_millis(500);
                }
                attempt += 1;
                if attempt >= MAX_ATTEMPTS {
                    return Err(e.context(format!("bundle {} failed after {MAX_ATTEMPTS} attempts", hex::encode(req.bundle))));
                }
                tracing::warn!("bundle {}: {e:#}; retrying in {delay:?}", hex::encode(req.bundle));
                tokio::select! {
                    _ = ctx.cancel.cancelled() => return Err(CytrusError::Cancelled.into()),
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
    }
}

fn is_cancel(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<CytrusError>(), Some(CytrusError::Cancelled))
}

async fn fetch_once(ctx: &FetchCtx, url: &str, remaining: &mut Vec<WantedChunk>) -> Result<(), FetchErr> {
    if remaining.is_empty() {
        return Ok(());
    }
    let ranges = merge_ranges(remaining);
    let multi = ranges.len() > 1;
    let send = ctx.client.get(url).header(reqwest::header::RANGE, range_header(&ranges)).send();
    let resp = tokio::select! {
        _ = ctx.cancel.cancelled() => return Err(FetchErr::Fatal(CytrusError::Cancelled.into())),
        r = tokio::time::timeout(IDLE_TIMEOUT, send) => r,
    };
    let resp = match resp {
        Err(_) => return Err(FetchErr::Retry(anyhow!("GET {url}: timeout"))),
        Ok(Err(e)) => return Err(FetchErr::Retry(anyhow::Error::new(e).context(format!("GET {url}")))),
        Ok(Ok(r)) => r,
    };
    let status = resp.status();
    enum Mode {
        Multi(MultipartParser),
        Single(u64),
    }
    let mut mode = match status.as_u16() {
        206 => {
            let ct = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
            if let Some(b) = multipart_boundary(ct) {
                Mode::Multi(MultipartParser::new(&b))
            } else {
                let cr = resp.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(parse_content_range);
                match cr {
                    Some((a, _)) => Mode::Single(a),
                    None => return Err(FetchErr::Retry(anyhow!("GET {url}: 206 without Content-Range"))),
                }
            }
        }
        200 => {
            if multi {
                ctx.multi_range_ok.store(false, Ordering::Relaxed);
                tracing::warn!("server ignored multi-range request; switching to single ranges");
            }
            Mode::Single(0)
        }
        s if s >= 500 || s == 408 || s == 416 || s == 429 || s == 404 || s == 403 => {
            return Err(FetchErr::Retry(anyhow!("GET {url}: HTTP {status}")));
        }
        _ => return Err(FetchErr::Fatal(anyhow!("GET {url}: unexpected HTTP {status}"))),
    };

    let mut collector = ChunkCollector::new(std::mem::take(remaining));
    let mut stream = resp.bytes_stream();
    let mut completed = Vec::new();
    let result: Result<(), FetchErr> = async {
        loop {
            if collector.all_done() {
                return Ok(());
            }
            let next = tokio::select! {
                _ = ctx.cancel.cancelled() => return Err(FetchErr::Fatal(CytrusError::Cancelled.into())),
                n = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => n,
            };
            let data = match next {
                Err(_) => return Err(FetchErr::Retry(anyhow!("GET {url}: read timeout"))),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(FetchErr::Retry(anyhow::Error::new(e).context(format!("GET {url}: body")))),
                Ok(Some(Ok(d))) => d,
            };
            if let Some(l) = &ctx.limiter {
                l.consume(data.len()).await;
            }
            ctx.received.fetch_add(data.len() as u64, Ordering::Relaxed);
            match &mut mode {
                Mode::Single(pos) => {
                    collector.feed(*pos, &data, &mut completed);
                    *pos += data.len() as u64;
                }
                Mode::Multi(p) => {
                    let mut err = None;
                    p.feed(&data, &mut |off, seg| collector.feed(off, seg, &mut completed)).unwrap_or_else(|e| err = Some(e));
                    if let Some(e) = err {
                        return Err(FetchErr::Retry(e));
                    }
                }
            }
            if !completed.is_empty() {
                let batch = std::mem::take(&mut completed);
                let sink = ctx.sink.clone();
                let delivered = ctx.delivered.clone();
                let r = tokio::task::spawn_blocking(move || -> Result<Vec<WantedChunk>, FetchErr> {
                    let mut bad = Vec::new();
                    for (c, bytes) in batch {
                        let got: Hash = sha1::Sha1::digest(&bytes).into();
                        if got != c.hash {
                            bad.push(c);
                            continue;
                        }
                        sink.write(&c.hash, &bytes).map_err(FetchErr::Fatal)?;
                        delivered.fetch_add(c.size, Ordering::Relaxed);
                    }
                    Ok(bad)
                })
                .await
                .map_err(|e| FetchErr::Fatal(e.into()))??;
                if !r.is_empty() {
                    // Keep them in `remaining` (collector marked them done); report a retryable error.
                    let n = r.len();
                    remaining.extend(r);
                    return Err(FetchErr::Retry(anyhow!("{n} chunk(s) failed SHA-1 verification")));
                }
            }
        }
        if collector.all_done() { Ok(()) } else { Err(FetchErr::Retry(anyhow!("GET {url}: response ended early"))) }
    }
    .await;
    remaining.extend(collector.remaining());
    remaining.sort_by_key(|c| c.offset);
    remaining.dedup_by_key(|c| c.offset);
    match result {
        Err(FetchErr::Fatal(e)) if is_cancel(&e) => Err(FetchErr::Fatal(e)),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wc(offset: u64, size: u64) -> WantedChunk {
        WantedChunk { hash: [offset as u8; 20], offset, size }
    }

    #[test]
    fn merges_adjacent_ranges() {
        let r = merge_ranges(&[wc(0, 10), wc(10, 5), wc(20, 5), wc(25, 1)]);
        assert_eq!(r, vec![(0, 14), (20, 25)]);
        assert_eq!(range_header(&r), "bytes=0-14,20-25");
    }

    #[test]
    fn splits_requests_by_header_length() {
        // 2000 non-adjacent chunks => many ranges; each request header must stay <= limit.
        let chunks: Vec<_> = (0..2000u64).map(|i| wc(i * 100_000, 50_000)).collect();
        let reqs = plan_requests([1; 20], chunks.clone(), MAX_RANGE_HEADER, u64::MAX);
        assert!(reqs.len() > 1);
        let mut total = 0;
        for r in &reqs {
            let h = range_header(&merge_ranges(&r.chunks));
            assert!(h.len() <= MAX_RANGE_HEADER, "{}", h.len());
            total += r.chunks.len();
        }
        assert_eq!(total, 2000);
        // Requests are nearly full (greedy).
        let h0 = range_header(&merge_ranges(&reqs[0].chunks));
        assert!(h0.len() > MAX_RANGE_HEADER - 30);
    }

    #[test]
    fn splits_requests_by_bytes_and_keeps_adjacent_merge() {
        let chunks: Vec<_> = (0..10u64).map(|i| wc(i * 10, 10)).collect();
        let reqs = plan_requests([1; 20], chunks, MAX_RANGE_HEADER, 35);
        assert_eq!(reqs.iter().map(|r| r.chunks.len()).collect::<Vec<_>>(), vec![3, 3, 3, 1]);
        assert_eq!(merge_ranges(&reqs[0].chunks), vec![(0, 29)]);
        // One oversized chunk still gets its own request.
        let reqs = plan_requests([1; 20], vec![wc(0, 100), wc(100, 5)], MAX_RANGE_HEADER, 10);
        assert_eq!(reqs.len(), 2);
    }

    #[test]
    fn header_length_accounting_is_exact() {
        // Contiguous extension across a digit boundary (9 -> 10..) must be accounted.
        let chunks = vec![wc(0, 5), wc(5, 5), wc(10, 90), wc(200, 5)];
        let reqs = plan_requests([1; 20], chunks, "bytes=0-99,200-204".len(), u64::MAX);
        assert_eq!(reqs.len(), 1);
        let reqs = plan_requests([1; 20], vec![wc(0, 5), wc(5, 5), wc(10, 90), wc(200, 5)], "bytes=0-99,200-204".len() - 1, u64::MAX);
        assert_eq!(reqs.len(), 2);
    }

    #[test]
    fn content_range_and_boundary() {
        assert_eq!(parse_content_range("bytes 0-2350/7879"), Some((0, 2350)));
        assert_eq!(parse_content_range(" bytes 10-20/*"), Some((10, 20)));
        assert_eq!(parse_content_range("bytes 20-10/30"), None);
        assert_eq!(multipart_boundary("multipart/byteranges; boundary=00000000003734884208").as_deref(), Some("00000000003734884208"));
        assert_eq!(multipart_boundary("multipart/byteranges; boundary=\"abc\"").as_deref(), Some("abc"));
        assert_eq!(multipart_boundary("application/octet-stream"), None);
    }

    fn sample(p: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cytrus").join(p)).unwrap()
    }

    fn parse_all(body: &[u8], boundary: &str, step: usize) -> Vec<(u64, Vec<u8>)> {
        let mut p = MultipartParser::new(boundary);
        let mut parts: Vec<(u64, Vec<u8>)> = Vec::new();
        for piece in body.chunks(step) {
            p.feed(piece, &mut |off, seg| match parts.last_mut() {
                Some((o, d)) if *o + d.len() as u64 == off => d.extend_from_slice(seg),
                _ => parts.push((off, seg.to_vec())),
            })
            .unwrap();
        }
        assert!(p.is_done());
        parts
    }

    #[test]
    fn parses_real_multirange_sample_at_any_split() {
        let body = sample("bundle-dofus3-config/multirange.bin");
        let full = sample("bundle-dofus3-config/full.bin");
        let hdr = String::from_utf8(sample("bundle-dofus3-config/multirange.headers.txt")).unwrap();
        let ct = hdr.lines().find(|l| l.to_ascii_lowercase().starts_with("content-type")).unwrap();
        let boundary = multipart_boundary(ct.split_once(':').unwrap().1).unwrap();
        for step in [1, 2, 3, 7, 64, 1000, body.len()] {
            let parts = parse_all(&body, &boundary, step);
            // The two ranges (0-2350, 2351-7878) are contiguous, so the helper merges them.
            assert_eq!(parts.len(), 1, "step {step}");
            assert_eq!(parts[0].0, 0);
            let mut covered = 0;
            for (off, data) in &parts {
                assert_eq!(&full[*off as usize..*off as usize + data.len()], &data[..]);
                covered += data.len();
            }
            assert_eq!(covered, full.len());
        }
    }

    #[test]
    fn multipart_without_leading_crlf_and_lf_only() {
        let body = b"--XX\r\nContent-Range: bytes 5-7/100\r\n\r\nabc\r\n--XX\nContent-Type: x\ncontent-range: bytes 0-1/100\n\nzz\r\n--XX--\r\n";
        let parts = parse_all(body, "XX", 1);
        assert_eq!(parts, vec![(5, b"abc".to_vec()), (0, b"zz".to_vec())]);
    }

    #[test]
    fn collector_assembles_and_verifies_order() {
        let full = sample("bundle-dofus3-config/full.bin");
        let m = super::super::manifest::parse(&sample("dofus-dofus3-windows-6.0_3.7.10.10.config")).unwrap();
        let chunks: Vec<_> = m.fragments[0].bundles[0].chunks.iter().map(|c| WantedChunk { hash: c.hash, offset: c.offset, size: c.size }).collect();
        let mut col = ChunkCollector::new(chunks.clone());
        let mut out = Vec::new();
        for (i, piece) in full.chunks(100).enumerate() {
            col.feed(i as u64 * 100, piece, &mut out);
        }
        assert!(col.all_done());
        assert_eq!(out.len(), 2);
        for (c, data) in out {
            let h: Hash = sha1::Sha1::digest(&data).into();
            assert_eq!(h, c.hash);
        }
        // Data with a gap leaves the chunk incomplete.
        let mut col = ChunkCollector::new(chunks);
        let mut out = Vec::new();
        col.feed(10, &full[10..], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(col.remaining().len(), 1);
        assert_eq!(col.remaining()[0].offset, 0);
    }

    struct Rec(std::sync::Mutex<Vec<Hash>>);
    impl ChunkSink for Rec {
        fn write(&self, hash: &Hash, _data: &[u8]) -> Result<()> {
            self.0.lock().unwrap().push(*hash);
            Ok(())
        }
    }

    /// Live CDN: non-adjacent chunks of a real bundle via one multi-range request, then via the
    /// single-range fallback. Needs the cached dofus3 manifest (run the integration test first).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "network: real Ankama CDN"]
    async fn live_multirange_and_fallback() {
        let remote = crate::cytrus::resolve("dofus3").await.unwrap();
        let path = crate::paths::data_dir().join("cache").join(format!("dofus3-{}.manifest", remote.version));
        let m = super::super::manifest::parse(&std::fs::read(path).unwrap()).unwrap();
        let b = m.fragment("skins").unwrap().bundles.iter().find(|b| b.chunks.len() >= 8).unwrap();
        let mut sorted = b.chunks.clone();
        sorted.sort_by_key(|c| c.offset);
        let want: Vec<WantedChunk> = [1usize, 3, 6].iter().map(|&i| WantedChunk { hash: sorted[i].hash, offset: sorted[i].offset, size: sorted[i].size }).collect();
        for multi in [true, false] {
            let rec = Arc::new(Rec(Default::default()));
            let ctx = FetchCtx {
                client: crate::http::client().clone(),
                cancel: CancellationToken::new(),
                limiter: None,
                sink: rec.clone(),
                received: Default::default(),
                delivered: Default::default(),
                multi_range_ok: Arc::new(AtomicBool::new(multi)),
                url_for: crate::cytrus::remote::bundle_url,
            };
            fetch_request(&ctx, RangeRequest { bundle: b.hash, chunks: want.clone() }).await.unwrap();
            let mut got = rec.0.lock().unwrap().clone();
            got.sort();
            let mut exp: Vec<Hash> = want.iter().map(|c| c.hash).collect();
            exp.sort();
            assert_eq!(got, exp);
            assert!(ctx.multi_range_ok.load(Ordering::Relaxed) == multi, "server answered multi-range with 200");
            eprintln!("multi={multi}: received {} bytes for {} wanted", ctx.received.load(Ordering::Relaxed), want.iter().map(|c| c.size).sum::<u64>());
        }
    }
}
