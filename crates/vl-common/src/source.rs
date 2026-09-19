//! Source-text table. Stages pass `&str` + [`Span`]s around;
//! the driver owns the [`Sources`] db so filenames stay attached to bytes.

/// Index into [`Sources`].
pub type FileId = usize;

/// One input file.
#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub text: String,
}

/// Minimal file table. Single-file programs use id `0`.
#[derive(Debug, Default)]
pub struct Sources {
    files: Vec<Source>,
}

impl Sources {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> FileId {
        let id = self.files.len();
        self.files.push(Source {
            name: name.into(),
            text: text.into(),
        });
        id
    }

    pub fn get(&self, id: FileId) -> Option<&Source> {
        self.files.get(id)
    }

    pub fn text(&self, id: FileId) -> Option<&str> {
        self.get(id).map(|s| s.text.as_str())
    }

    pub fn name(&self, id: FileId) -> Option<&str> {
        self.get(id).map(|s| s.name.as_str())
    }
}
