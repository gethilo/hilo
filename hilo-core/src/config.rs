use crate::manifest::Manifest;

/// Configuration builder wrapping a parsed manifest.
pub struct Config {
    pub manifest: Manifest,
}

impl Config {
    pub fn new(manifest: Manifest) -> Self {
        Self { manifest }
    }

    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let manifest = Manifest::from_file(path)?;
        Ok(Self::new(manifest))
    }
}
