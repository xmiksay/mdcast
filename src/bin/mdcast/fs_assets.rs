//! Minimal filesystem-backed `AssetProvider`, used by the CLI's `--assets`
//! flag. Keys map to relative paths inside `root`. Library callers should
//! impl the trait themselves for real overrides (DB, S3, in-memory map …).

use std::path::PathBuf;

use bytes::Bytes;
use mdcast::AssetProvider;

pub struct FsAssets(pub PathBuf);

impl AssetProvider for FsAssets {
    fn get<'a>(&'a self, key: &'a str) -> mdcast::BoxFuture<'a, anyhow::Result<Option<Bytes>>> {
        Box::pin(async move {
            let path = self.0.join(key);
            match tokio::fs::read(&path).await {
                Ok(b) => Ok(Some(Bytes::from(b))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> mdcast::BoxFuture<'a, anyhow::Result<Vec<String>>> {
        Box::pin(async move {
            let mut out = Vec::new();
            let mut dirs = std::collections::VecDeque::from([self.0.clone()]);
            while let Some(dir) = dirs.pop_front() {
                let mut entries = match tokio::fs::read_dir(&dir).await {
                    Ok(entries) => entries,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                while let Some(entry) = entries.next_entry().await? {
                    let path = entry.path();
                    if entry.file_type().await?.is_dir() {
                        dirs.push_back(path);
                        continue;
                    }
                    let rel = path.strip_prefix(&self.0).expect("walked under root");
                    let key = rel
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/");
                    if key.starts_with(prefix) {
                        out.push(key);
                    }
                }
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_walks_subdirectories_and_filters_by_prefix() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("revealjs/dist"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("revealjs/dist/reveal.js"), b"js")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("theme.css"), b"css")
            .await
            .unwrap();

        let assets = FsAssets(dir.path().to_path_buf());
        let mut keys = assets.list("revealjs/").await.unwrap();
        keys.sort();
        assert_eq!(keys, vec!["revealjs/dist/reveal.js".to_string()]);
    }

    #[tokio::test]
    async fn list_on_missing_directory_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let assets = FsAssets(dir.path().join("does-not-exist"));
        assert!(assets.list("").await.unwrap().is_empty());
    }
}
