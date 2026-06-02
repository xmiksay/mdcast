//! Backend registry. Per-target dispatch sits in `Registry::render`; backends
//! themselves are feature-gated so a consumer can build with only what they need.

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::{Artifact, Backend, RenderRequest, Target};

#[cfg(feature = "pandoc")]
pub mod pandoc;
#[cfg(feature = "typst")]
pub mod typst;

pub struct Registry {
    backends: HashMap<Target, Box<dyn Backend>>,
}

impl Registry {
    pub fn empty() -> Self {
        Self { backends: HashMap::new() }
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

    pub async fn render(&self, target: Target, req: &RenderRequest<'_>) -> Result<Artifact> {
        let Some(b) = self.backends.get(&target) else {
            bail!("no backend registered for target {:?}", target);
        };
        b.render(req).await
    }
}
