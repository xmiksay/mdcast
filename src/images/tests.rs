use super::*;
use crate::assets::sync_provider;
use crate::pages::PageOrigin;
use bytes::Bytes;

fn page(body: &str) -> Page {
    Page {
        class: "content".into(),
        body: body.into(),
        origin: PageOrigin::Explicit,
    }
}

#[tokio::test]
async fn rewrites_resolved_keys_only() {
    let provider = sync_provider(|key| match key {
        "img/diagram.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
        _ => Ok(None),
    });
    let tmp = tempfile::tempdir().unwrap();
    let mut pages = vec![page(
        "![d](img/diagram.svg) and ![x](img/missing.png) and ![ext](https://img.invalid/p.png)",
    )];
    let resolved = resolve_images(&mut pages, &provider, tmp.path(), Target::Docx)
        .await
        .unwrap();

    assert_eq!(resolved, vec!["img/diagram.svg".to_string()]);
    assert!(pages[0].body.contains("img__diagram.svg"));
    assert!(
        pages[0].body.contains("img/missing.png"),
        "missing keys preserved"
    );
    assert!(
        pages[0].body.contains("https://img.invalid/p.png"),
        "remote URLs preserved when remote-images feature is off (and unresolvable when on — `.invalid` never resolves per RFC 2606)"
    );
}

#[tokio::test]
async fn resolves_titled_image() {
    let provider = sync_provider(|key| match key {
        "diagram.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
        _ => Ok(None),
    });
    let tmp = tempfile::tempdir().unwrap();
    let mut pages = vec![page(r#"![d](diagram.svg "Fig 1")"#)];
    let resolved = resolve_images(&mut pages, &provider, tmp.path(), Target::Docx)
        .await
        .unwrap();

    assert_eq!(resolved, vec!["diagram.svg".to_string()]);
    assert!(pages[0].body.contains("diagram.svg"), "{}", pages[0].body);
}

#[tokio::test]
async fn resolves_angle_bracket_url() {
    let provider = sync_provider(|key| match key {
        "diagram.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
        _ => Ok(None),
    });
    let tmp = tempfile::tempdir().unwrap();
    let mut pages = vec![page("![d](<diagram.svg>)")];
    let resolved = resolve_images(&mut pages, &provider, tmp.path(), Target::Docx)
        .await
        .unwrap();

    assert_eq!(resolved, vec!["diagram.svg".to_string()]);
    assert!(pages[0].body.contains("diagram.svg"), "{}", pages[0].body);
}

#[tokio::test]
async fn resolves_reference_style_image() {
    let provider = sync_provider(|key| match key {
        "diagram.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
        _ => Ok(None),
    });
    let tmp = tempfile::tempdir().unwrap();
    let mut pages = vec![page("![d][ref]\n\n[ref]: diagram.svg \"Fig 1\"\n")];
    let resolved = resolve_images(&mut pages, &provider, tmp.path(), Target::Docx)
        .await
        .unwrap();

    assert_eq!(resolved, vec!["diagram.svg".to_string()]);
    assert!(pages[0].body.contains("diagram.svg"), "{}", pages[0].body);
    assert!(
        !pages[0].body.contains("![d][ref]"),
        "reference-style form should be normalised to inline: {}",
        pages[0].body
    );
}

#[tokio::test]
async fn dedup_fetches_same_key_only_once() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = calls.clone();
    let provider = sync_provider(move |k| {
        calls_c.fetch_add(1, Ordering::SeqCst);
        if k == "shared.png" {
            Ok(Some(Bytes::from_static(b"x")))
        } else {
            Ok(None)
        }
    });
    let tmp = tempfile::tempdir().unwrap();
    let mut pages = vec![page("![](shared.png)"), page("![](shared.png)")];
    resolve_images(&mut pages, &provider, tmp.path(), Target::Docx)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Binds a local one-shot HTTP server, replies with the given status
/// line/body to the first request, and returns its URL — lets
/// `remote-images` tests exercise `reqwest` against something real
/// without reaching the network.
#[cfg(feature = "remote-images")]
async fn serve_once(status_line: &'static str, body: &'static [u8]) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await;
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.write_all(body).await;
        let _ = socket.shutdown().await;
    });
    format!("http://{addr}/image.png")
}

#[tokio::test]
#[cfg(feature = "remote-images")]
async fn remote_images_feature_fetches_http_url() {
    let url = serve_once("200 OK", b"PNGDATA").await;
    let provider = sync_provider(|_| Ok(None));
    let tmp = tempfile::tempdir().unwrap();
    let mut pages = vec![page(&format!("![r]({url})"))];

    let resolved = resolve_images(&mut pages, &provider, tmp.path(), Target::Docx)
        .await
        .unwrap();

    assert_eq!(resolved, vec![url.clone()]);
    assert!(
        !pages[0].body.contains(&url),
        "remote url should be rewritten to a local path: {}",
        pages[0].body
    );
}

#[tokio::test]
#[cfg(feature = "remote-images")]
async fn remote_images_feature_warns_and_skips_on_error_status() {
    let url = serve_once("404 Not Found", b"").await;
    let provider = sync_provider(|_| Ok(None));
    let tmp = tempfile::tempdir().unwrap();
    let mut pages = vec![page(&format!("![r]({url})"))];

    let resolved = resolve_images(&mut pages, &provider, tmp.path(), Target::Docx)
        .await
        .unwrap();

    assert!(resolved.is_empty());
    assert!(
        pages[0].body.contains(&url),
        "unresolved remote url left in place: {}",
        pages[0].body
    );
}

#[tokio::test]
#[cfg(feature = "remote-images")]
async fn remote_images_feature_dedups_same_url_across_pages() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_c = hits.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            hits_c.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let body = b"PNGDATA";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.write_all(body).await;
            let _ = socket.shutdown().await;
        }
    });
    let url = format!("http://{addr}/shared.png");

    let provider = sync_provider(|_| Ok(None));
    let tmp = tempfile::tempdir().unwrap();
    let mut pages = vec![page(&format!("![a]({url})")), page(&format!("![b]({url})"))];

    resolve_images(&mut pages, &provider, tmp.path(), Target::Docx)
        .await
        .unwrap();

    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "expected a single fetch for a repeated remote url"
    );
}
