//! What a successful render hands back: the bytes, plus the two things the
//! response headers say about them.

use bytes::Bytes;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, HeaderMap};

/// A rendered document.
///
/// `filename` and `content_type` come from `Content-Disposition` and
/// `Content-Type`. Both are always present on an `mdcast-server` response; the
/// fallbacks exist for an intermediary that strips them, and are derived (the
/// target's own extension, HTTP's default media type) rather than invented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub bytes: Bytes,
    pub filename: String,
    pub content_type: String,
}

/// HTTP's own default when a response carries no `Content-Type`.
const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

pub(crate) fn from_response(headers: &HeaderMap, bytes: Bytes, fallback_name: &str) -> Artifact {
    let header = |name: &_| headers.get(name).and_then(|v| v.to_str().ok());

    Artifact {
        bytes,
        filename: header(&CONTENT_DISPOSITION)
            .and_then(filename_from)
            .unwrap_or_else(|| fallback_name.to_string()),
        content_type: header(&CONTENT_TYPE)
            .unwrap_or(DEFAULT_CONTENT_TYPE)
            .to_string(),
    }
}

/// Pull the filename out of `attachment; filename="output.pdf"`.
///
/// A response filename is data from the network, and callers will write it to
/// disk — so anything with a path separator in it, or a bare `.`/`..`, is
/// discarded in favour of the derived fallback rather than sanitised into
/// something the caller did not ask for. RFC 5987's `filename*=` is not
/// handled: mdcast-server always mints a plain ASCII `output.<ext>`.
fn filename_from(header: &str) -> Option<String> {
    let (_, rest) = header.split_once("filename=")?;
    let rest = rest.trim_start();
    let name = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or(quoted),
        None => rest.split(';').next().unwrap_or(rest),
    };

    let name = name.trim();
    let usable = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\');
    usable.then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn artifact(pairs: &[(&'static str, &str)]) -> Artifact {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            let value = HeaderValue::from_str(value).expect("test header is valid ASCII");
            headers.insert(*name, value);
        }
        from_response(&headers, Bytes::from_static(b"%PDF"), "output.pdf")
    }

    #[test]
    fn takes_the_filename_and_content_type_from_the_headers() {
        let artifact = artifact(&[
            ("content-type", "application/pdf"),
            ("content-disposition", "attachment; filename=\"deck.pdf\""),
        ]);

        assert_eq!(artifact.filename, "deck.pdf");
        assert_eq!(artifact.content_type, "application/pdf");
        assert_eq!(&artifact.bytes[..], b"%PDF");
    }

    #[test]
    fn accepts_an_unquoted_filename() {
        assert_eq!(
            artifact(&[("content-disposition", "attachment; filename=deck.pdf")]).filename,
            "deck.pdf"
        );
        assert_eq!(
            artifact(&[(
                "content-disposition",
                "attachment; filename=deck.pdf; charset=utf-8"
            )])
            .filename,
            "deck.pdf"
        );
    }

    #[test]
    fn falls_back_when_the_headers_say_nothing() {
        let artifact = artifact(&[]);

        assert_eq!(artifact.filename, "output.pdf");
        assert_eq!(artifact.content_type, DEFAULT_CONTENT_TYPE);
    }

    #[test]
    fn a_filename_with_a_path_in_it_is_discarded() {
        for header in [
            "attachment; filename=\"../../etc/passwd\"",
            "attachment; filename=/etc/passwd",
            "attachment; filename=\"..\\\\windows\\\\system32\"",
            "attachment; filename=\"..\"",
            "attachment; filename=\"\"",
            "attachment",
        ] {
            assert_eq!(
                artifact(&[("content-disposition", header)]).filename,
                "output.pdf",
                "accepted a filename from {header:?}"
            );
        }
    }
}
