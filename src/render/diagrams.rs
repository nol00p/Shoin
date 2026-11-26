//! The Mermaid renderer as the editor asks it: [`MermaidRenderer`], or in
//! tests a stub in its place whose results arrive when the test says, not
//! when a worker thread happens to finish. The random-session fuzz runs on
//! the stub, so a seed replays exactly; the renderer's own scheduling is
//! tested with real workers elsewhere (`app::mermaid_tests`).

use crate::render::mermaid::{MermaidRenderer, PicturePreview, Preview};

/// The renderer the panes share (`App::mermaid`).
#[derive(Debug, Default)]
pub(crate) struct Diagrams {
    renderer: MermaidRenderer,
    #[cfg(test)]
    stub: Option<stub::Stub>,
}

impl Diagrams {
    /// A renderer whose results arrive only at [`Diagrams::complete`],
    /// rendered by `render`, with `png` as every picture.
    #[cfg(test)]
    pub(crate) fn stub(render: fn(&str) -> Preview, png: std::sync::Arc<[u8]>) -> Self {
        Diagrams { renderer: MermaidRenderer::default(), stub: Some(stub::Stub::new(render, png)) }
    }

    /// Every result asked for so far, now: the stub's way of finishing its
    /// jobs. True when there were any.
    #[cfg(test)]
    pub(crate) fn complete(&mut self) -> bool {
        self.stub.as_mut().is_some_and(stub::Stub::complete)
    }

    /// Forget every result, as a configuration reload does. A stub stays
    /// one.
    pub(crate) fn reset(&mut self) {
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            *self = Diagrams::stub(stub.render, stub.png.clone());
            return;
        }
        *self = Diagrams::default();
    }

    /// Take the results that came in; true when anything changed.
    pub(crate) fn poll(&mut self) -> bool {
        #[cfg(test)]
        if self.stub.is_some() {
            return false;
        }
        self.renderer.poll()
    }

    pub(crate) fn cached(&self, executable: &str, source: &str) -> Option<Preview> {
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            return stub.cached(source, false).map(|r| r.text());
        }
        self.renderer.cached(executable, source)
    }

    pub(crate) fn cached_png(&self, executable: &str, source: &str) -> Option<PicturePreview> {
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            return stub.cached(source, true).map(|r| r.png());
        }
        self.renderer.cached_png(executable, source)
    }

    pub(crate) fn request(&mut self, executable: &str, source: &str) -> Preview {
        #[cfg(test)]
        if let Some(stub) = &mut self.stub {
            return stub.request(source, false).text();
        }
        self.renderer.request(executable, source)
    }

    pub(crate) fn request_png(&mut self, executable: &str, source: &str) -> PicturePreview {
        #[cfg(test)]
        if let Some(stub) = &mut self.stub {
            return stub.request(source, true).png();
        }
        self.renderer.request_png(executable, source)
    }

    /// The pictures rendered so far, for an export to start from.
    pub(crate) fn with_cached_pngs(&self) -> MermaidRenderer {
        self.renderer.with_cached_pngs()
    }
}

#[cfg(test)]
mod stub {
    use crate::render::mermaid::{
        Failure, PicturePreview, Preview, RenderState, MAX_CACHE_ENTRIES, MAX_SOURCE_BYTES,
    };
    use std::collections::VecDeque;
    use std::sync::Arc;

    /// As many jobs at once as the renderer runs.
    const JOBS: usize = 2;

    /// A result of either kind.
    #[derive(Clone, Debug)]
    pub(super) enum Result {
        Text(Preview),
        Png(PicturePreview),
        Pending,
    }

    impl Result {
        pub(super) fn text(self) -> Preview {
            match self {
                Result::Text(preview) => preview,
                Result::Pending => Preview::Pending,
                Result::Png(_) => unreachable!("a text request"),
            }
        }

        pub(super) fn png(self) -> PicturePreview {
            match self {
                Result::Png(preview) => preview,
                Result::Pending => PicturePreview::Pending,
                Result::Text(_) => unreachable!("a picture request"),
            }
        }
    }

    /// The renderer's admission and cache, as `MermaidRenderer` keeps them
    /// (a hit moves to the back, a result past the capacity drops the
    /// front, at most [`JOBS`] at once), with jobs that finish together at
    /// [`Stub::complete`].
    #[derive(Debug)]
    pub(super) struct Stub {
        pub(super) render: fn(&str) -> Preview,
        pub(super) png: Arc<[u8]>,
        /// Per kind, text then picture: the sources rendered, oldest first.
        caches: [VecDeque<(String, Result)>; 2],
        jobs: Vec<(String, bool)>,
    }

    impl Stub {
        pub(super) fn new(render: fn(&str) -> Preview, png: Arc<[u8]>) -> Self {
            Stub { render, png, caches: Default::default(), jobs: Vec::new() }
        }

        pub(super) fn cached(&self, source: &str, png: bool) -> Option<Result> {
            self.caches[usize::from(png)].iter().find(|(s, _)| s == source).map(|(_, r)| r.clone())
        }

        pub(super) fn request(&mut self, source: &str, png: bool) -> Result {
            if source.len() > MAX_SOURCE_BYTES {
                let failed = Failure::TooLarge;
                return if png { Result::Png(RenderState::Failed(failed)) } else { Result::Text(RenderState::Failed(failed)) };
            }
            let cache = &mut self.caches[usize::from(png)];
            if let Some(i) = cache.iter().position(|(s, _)| s == source) {
                let entry = cache.remove(i).expect("found");
                let result = entry.1.clone();
                cache.push_back(entry);
                return result;
            }
            if self.jobs.len() < JOBS && !self.jobs.iter().any(|(s, p)| s == source && *p == png) {
                self.jobs.push((source.to_string(), png));
            }
            Result::Pending
        }

        pub(super) fn complete(&mut self) -> bool {
            let jobs = std::mem::take(&mut self.jobs);
            for (source, png) in &jobs {
                let result = if *png {
                    Result::Png(RenderState::Ready(self.png.clone()))
                } else {
                    Result::Text((self.render)(source))
                };
                let cache = &mut self.caches[usize::from(*png)];
                if cache.len() == MAX_CACHE_ENTRIES {
                    cache.pop_front();
                }
                cache.push_back((source.clone(), result));
            }
            !jobs.is_empty()
        }
    }
}
