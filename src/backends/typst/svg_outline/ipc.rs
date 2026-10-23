//! Framing between the parent and the SVG-outline child (`child.rs`): plain
//! length-prefixed binary, little-endian, no dependency.
//!
//! ```text
//! request  = "MDO1" u32:n_files { u32:len path  u64:len bytes }*
//!                   u32:n_fonts { u64:len bytes }*
//! response = "MDR1" u32:n { u8:tag [payload] }*
//!   tag 0 Keep | 1 Replace u64:len svg | 2 Drop u32:len reason
//!       | 3 Unparsed u32:len reason
//! ```
//!
//! Decoding never trusts a length: each one is checked against the bytes
//! actually left in the buffer (which the parent already capped while
//! reading the child's stdout) before anything is sliced or allocated, and a
//! replacement SVG or reason past its own cap is rejected outright — a broken
//! or hostile child cannot make the parent allocate beyond that cap.

use anyhow::{Result, bail, ensure};

use super::Outcome;

const REQUEST_MAGIC: &[u8; 4] = b"MDO1";
const RESPONSE_MAGIC: &[u8; 4] = b"MDR1";
/// usvg's error messages are one line; anything longer is truncated on
/// encode so a valid child never trips the decode check.
const MAX_REASON: usize = 64 << 10;

/// Borrows every path and payload from the request buffer: the child runs
/// under the same `RLIMIT_AS` as the compile, so a second in-memory copy of
/// the render's inputs would halve what it can take.
pub(super) struct Request<'a> {
    pub files: Vec<(&'a str, &'a [u8])>,
    pub fonts: Vec<&'a [u8]>,
}

pub(super) fn encode_request(files: &[(&str, &[u8])], fonts: &[&[u8]]) -> Vec<u8> {
    let payload: usize = files
        .iter()
        .map(|(p, b)| p.len() + b.len() + 12)
        .sum::<usize>()
        + fonts.iter().map(|f| f.len() + 8).sum::<usize>();
    let mut out = Vec::with_capacity(payload + 12);
    out.extend_from_slice(REQUEST_MAGIC);
    put_u32(&mut out, files.len());
    for (path, bytes) in files {
        put_u32(&mut out, path.len());
        out.extend_from_slice(path.as_bytes());
        put_u64(&mut out, bytes.len());
        out.extend_from_slice(bytes);
    }
    put_u32(&mut out, fonts.len());
    for font in fonts {
        put_u64(&mut out, font.len());
        out.extend_from_slice(font);
    }
    out
}

pub(super) fn decode_request(buf: &[u8]) -> Result<Request<'_>> {
    let mut r = Reader::new(buf, REQUEST_MAGIC)?;
    let n = r.u32()?;
    let mut files = Vec::new();
    for _ in 0..n {
        let len = r.u32()?;
        let path = std::str::from_utf8(r.take(len)?)?;
        let len = r.u64()?;
        files.push((path, r.take(len)?));
    }
    let n = r.u32()?;
    let mut fonts = Vec::new();
    for _ in 0..n {
        let len = r.u64()?;
        fonts.push(r.take(len)?);
    }
    r.finish()?;
    Ok(Request { files, fonts })
}

pub(super) fn encode_response(outcomes: &[Outcome]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(RESPONSE_MAGIC);
    put_u32(&mut out, outcomes.len());
    for outcome in outcomes {
        match outcome {
            Outcome::Keep => out.push(0),
            Outcome::Replace(svg) => {
                out.push(1);
                put_u64(&mut out, svg.len());
                out.extend_from_slice(svg);
            }
            Outcome::Drop(reason) => put_reason(&mut out, 2, reason),
            Outcome::Unparsed(reason) => put_reason(&mut out, 3, reason),
        }
    }
    out
}

/// Upper bound on a valid response for `n_files` files whose replacements
/// are each at most `max_svg` bytes — what the parent caps the child's
/// stdout at.
pub(super) fn response_cap(n_files: usize, max_svg: usize) -> usize {
    let per_file = 1 + 8 + max_svg.max(4 + MAX_REASON);
    8usize.saturating_add(n_files.saturating_mul(per_file))
}

pub(super) fn decode_response(buf: &[u8], n_files: usize, max_svg: usize) -> Result<Vec<Outcome>> {
    let mut r = Reader::new(buf, RESPONSE_MAGIC)?;
    let n = r.u32()?;
    ensure!(
        n == n_files,
        "outline child answered for {n} files, expected {n_files}"
    );
    let mut outcomes = Vec::with_capacity(n);
    for _ in 0..n {
        let outcome = match r.take(1)?[0] {
            0 => Outcome::Keep,
            1 => {
                let len = r.u64()?;
                ensure!(
                    len <= max_svg,
                    "outlined SVG of {len} bytes exceeds the {max_svg}-byte cap"
                );
                Outcome::Replace(r.take(len)?.to_vec())
            }
            2 => Outcome::Drop(r.reason()?),
            3 => Outcome::Unparsed(r.reason()?),
            tag => bail!("unknown outcome tag {tag}"),
        };
        outcomes.push(outcome);
    }
    r.finish()?;
    Ok(outcomes)
}

fn put_u32(out: &mut Vec<u8>, n: usize) {
    // Every count and path length here comes from an in-memory render input;
    // none approaches 4 GiB, so saturating is a formality, not a code path.
    out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&(n as u64).to_le_bytes());
}

fn put_reason(out: &mut Vec<u8>, tag: u8, reason: &str) {
    let mut end = reason.len().min(MAX_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    out.push(tag);
    put_u32(out, end);
    out.extend_from_slice(&reason.as_bytes()[..end]);
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8], magic: &[u8; 4]) -> Result<Self> {
        let mut r = Self { buf };
        ensure!(r.take(4)? == magic, "not an SVG-outline frame (bad magic)");
        Ok(r)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        ensure!(
            len <= self.buf.len(),
            "truncated SVG-outline frame: need {len} bytes, {} left",
            self.buf.len()
        );
        let (head, rest) = self.buf.split_at(len);
        self.buf = rest;
        Ok(head)
    }

    fn u32(&mut self) -> Result<usize> {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(b) as usize)
    }

    fn u64(&mut self) -> Result<usize> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(usize::try_from(u64::from_le_bytes(b))?)
    }

    fn reason(&mut self) -> Result<String> {
        let len = self.u32()?;
        ensure!(
            len <= MAX_REASON,
            "reason of {len} bytes exceeds the {MAX_REASON}-byte cap"
        );
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn finish(self) -> Result<()> {
        ensure!(
            self.buf.is_empty(),
            "{} trailing bytes after SVG-outline frame",
            self.buf.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("a.svg", b"<svg/>"),
            ("dir/ü.png", &[0, 1, 2]),
            ("empty", &[]),
        ]
    }

    #[test]
    fn request_round_trips_borrowing_from_the_buffer() {
        let fonts: Vec<&[u8]> = vec![b"OTTO", &[]];
        let buf = encode_request(&files(), &fonts);
        let req = decode_request(&buf).unwrap();
        assert_eq!(req.files, files());
        assert_eq!(req.fonts, fonts);
        let inside = buf.as_ptr_range();
        assert!(inside.contains(&req.files[0].1.as_ptr()));
        assert!(inside.contains(&req.fonts[0].as_ptr()));
    }

    #[test]
    fn response_round_trips() {
        let outcomes = vec![
            Outcome::Keep,
            Outcome::Replace(b"<svg><path/></svg>".to_vec()),
            Outcome::Drop("bad ↔ svg".into()),
            Outcome::Unparsed("not gzip".into()),
        ];
        let buf = encode_response(&outcomes);
        assert!(buf.len() <= response_cap(4, 64));
        assert_eq!(decode_response(&buf, 4, 64).unwrap(), outcomes);
    }

    #[test]
    fn overlong_reason_is_truncated_on_a_char_boundary() {
        let reason = "↔".repeat(MAX_REASON);
        let buf = encode_response(&[Outcome::Drop(reason)]);
        let [Outcome::Drop(got)] = &decode_response(&buf, 1, 0).unwrap()[..] else {
            panic!("expected one Drop");
        };
        assert!(got.len() <= MAX_REASON && got.chars().all(|c| c == '↔'));
    }

    #[test]
    fn every_truncation_is_rejected() {
        let req = encode_request(&files(), &[b"font"]);
        for len in 0..req.len() {
            assert!(
                decode_request(&req[..len]).is_err(),
                "prefix {len} accepted"
            );
        }
        let resp = encode_response(&[Outcome::Replace(b"<svg/>".to_vec()), Outcome::Keep]);
        for len in 0..resp.len() {
            assert!(
                decode_response(&resp[..len], 2, 64).is_err(),
                "prefix {len} accepted"
            );
        }
    }

    #[test]
    fn oversized_and_malformed_responses_are_rejected() {
        let replace = encode_response(&[Outcome::Replace(vec![b'x'; 65])]);
        assert!(
            decode_response(&replace, 1, 64).is_err(),
            "over the SVG cap"
        );

        let mut huge = RESPONSE_MAGIC.to_vec();
        huge.extend_from_slice(&1u32.to_le_bytes());
        huge.push(1);
        huge.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(
            decode_response(&huge, 1, usize::MAX).is_err(),
            "length past the buffer"
        );

        let mut reason = RESPONSE_MAGIC.to_vec();
        reason.extend_from_slice(&1u32.to_le_bytes());
        reason.push(2);
        reason.extend_from_slice(&(MAX_REASON as u32 + 1).to_le_bytes());
        reason.extend(std::iter::repeat_n(b'r', MAX_REASON + 1));
        assert!(
            decode_response(&reason, 1, 0).is_err(),
            "over the reason cap"
        );

        let ok = encode_response(&[Outcome::Keep]);
        assert!(decode_response(&ok, 2, 0).is_err(), "wrong count");
        let mut trailing = ok.clone();
        trailing.push(0);
        assert!(decode_response(&trailing, 1, 0).is_err(), "trailing bytes");
        let mut tag = ok.clone();
        *tag.last_mut().unwrap() = 9;
        assert!(decode_response(&tag, 1, 0).is_err(), "unknown tag");
        assert!(
            decode_response(b"MDO1\0\0\0\0", 0, 0).is_err(),
            "request magic"
        );
    }
}
