//! Backend registry. Per-target dispatch sits in `Registry::render`; backends
//! themselves are feature-gated so a consumer can build with only what they need.

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::{
    Artifact, AssetProvider, Backend, RenderRequest, RenderedArtifact, ResolvedDoc, Target,
};

#[cfg(feature = "pandoc")]
pub mod pandoc;
#[cfg(feature = "typst")]
pub mod typst;

pub struct Registry {
    backends: HashMap<Target, Box<dyn Backend>>,
}

impl Registry {
    pub fn empty() -> Self {
        Self {
            backends: HashMap::new(),
        }
    }

    /// Registry with every feature-enabled backend installed at its default target.
    #[allow(unused_mut)] // `r` is only mutated when at least one backend feature is on
    pub fn with_defaults() -> Self {
        let mut r = Self::empty();
        #[cfg(feature = "typst")]
        {
            r.register(Box::new(typst::TypstBackend::new(Target::Pdf)));
            r.register(Box::new(typst::TypstBackend::new(Target::PdfPresentation)));
        }
        #[cfg(feature = "pandoc")]
        {
            r.register(Box::new(pandoc::PandocBackend::new(Target::Docx)));
            r.register(Box::new(pandoc::PandocBackend::new(Target::Odt)));
            r.register(Box::new(pandoc::PandocBackend::new(Target::Pptx)));
            r.register(Box::new(pandoc::PandocBackend::new(Target::HtmlReveal)));
        }
        r
    }

    pub fn register(&mut self, backend: Box<dyn Backend>) {
        self.backends.insert(backend.target(), backend);
    }

    /// Render straight into memory — no temp dir, no file to clean up. The
    /// entry point for server embedders handing bytes back in a response.
    pub async fn render_to_bytes(
        &self,
        target: Target,
        doc: &ResolvedDoc,
        assets: &dyn AssetProvider,
    ) -> Result<RenderedArtifact> {
        let Some(b) = self.backends.get(&target) else {
            bail!("no backend registered for target {:?}", target);
        };
        b.render_to_bytes(doc, assets).await
    }

    /// Render to a file on disk. Implemented over `render_to_bytes` — one
    /// render path, this just adds the write.
    pub async fn render(&self, target: Target, req: &RenderRequest<'_>) -> Result<Artifact> {
        let artifact = self.render_to_bytes(target, req.doc, req.assets).await?;
        artifact.write_to(req.out).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::sync_provider;
    use crate::{BrandHandle, DocMeta};

    struct StubBackend {
        target: Target,
    }

    impl Backend for StubBackend {
        fn target(&self) -> Target {
            self.target
        }

        fn render_to_bytes<'a>(
            &'a self,
            _doc: &'a ResolvedDoc,
            _assets: &'a dyn AssetProvider,
        ) -> crate::BoxFuture<'a, Result<RenderedArtifact>> {
            Box::pin(async move {
                Ok(RenderedArtifact {
                    primary: bytes::Bytes::from_static(b"stub output"),
                    filename: "stub.txt".to_string(),
                    extras: Vec::new(),
                })
            })
        }
    }

    fn doc() -> ResolvedDoc {
        ResolvedDoc {
            pages: Vec::new(),
            meta: DocMeta::default(),
            brand: BrandHandle(std::sync::Arc::new(crate::BrandSpec::default())),
            assets: Vec::new(),
            toc: None,
        }
    }

    #[tokio::test]
    async fn render_to_bytes_dispatches_to_registered_backend() {
        let mut registry = Registry::empty();
        registry.register(Box::new(StubBackend {
            target: Target::Docx,
        }));
        let assets = sync_provider(|_| Ok(None));

        let artifact = registry
            .render_to_bytes(Target::Docx, &doc(), &assets)
            .await
            .unwrap();

        assert_eq!(artifact.primary, bytes::Bytes::from_static(b"stub output"));
        assert_eq!(artifact.filename, "stub.txt");
    }

    #[tokio::test]
    async fn render_to_bytes_unknown_target_errors() {
        let registry = Registry::empty();
        let assets = sync_provider(|_| Ok(None));

        let err = registry
            .render_to_bytes(Target::Pdf, &doc(), &assets)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("no backend registered for target"));
    }

    #[tokio::test]
    async fn render_writes_dispatched_bytes_to_disk() {
        let mut registry = Registry::empty();
        registry.register(Box::new(StubBackend {
            target: Target::Docx,
        }));
        let assets = sync_provider(|_| Ok(None));
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.docx");
        let d = doc();
        let req = RenderRequest {
            doc: &d,
            assets: &assets,
            out: &out,
        };

        let artifact = registry.render(Target::Docx, &req).await.unwrap();

        assert_eq!(artifact.primary, out);
        assert_eq!(
            tokio::fs::read(&out).await.unwrap(),
            b"stub output".to_vec()
        );
    }
}
