//! Corpus manifest format and on-disk layout.
//!
//! A corpus lives in a directory (default `bench/corpus`) containing:
//! - `manifest.toml` — the committed list of cases (see [`Manifest`]).
//! - `<case-id>/<variant>.wav` — 16 kHz mono recordings, produced by
//!   `handy-bench record` and NOT committed (the user's voice never leaves the
//!   machine; see `.gitignore`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default recording variants when a case does not specify its own.
pub fn default_variants() -> Vec<String> {
    vec![
        "normal".to_string(),
        "fast".to_string(),
        "noise".to_string(),
    ]
}

/// One benchmark case: a sentence the user reads aloud, plus the expected
/// formatted output the pipeline should produce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    /// Stable identifier; also the sub-directory name for this case's audio.
    pub id: String,
    /// The words the user speaks. Used as the WER/CER reference and shown as the
    /// prompt during `record`.
    pub spoken: String,
    /// The ideal formatted output after ASR + text rules. Used as the
    /// format-accuracy reference. May be empty for stub cases pending review.
    #[serde(default)]
    pub expected: String,
    /// Free-form tags for per-tag metric breakdowns (e.g. `de`, `punctuation`,
    /// `itn`, `tech`).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Audio variants to record for this case. Files live at
    /// `<case-id>/<variant>.wav`.
    #[serde(default = "default_variants")]
    pub variants: Vec<String>,
    /// Optional human note (known gaps, why `expected` is shaped a certain way).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The parsed `manifest.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    #[serde(default, rename = "case")]
    pub cases: Vec<Case>,
}

impl Manifest {
    /// Parse a manifest from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self> {
        toml::from_str(s).context("failed to parse corpus manifest TOML")
    }

    /// Serialize the manifest back to TOML (used by `export-history`).
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to serialize corpus manifest TOML")
    }
}

/// A corpus directory rooted at `root`, with `manifest.toml` alongside per-case
/// audio sub-directories.
pub struct Corpus {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Corpus {
    /// Path to the manifest file for a corpus rooted at `root`.
    pub fn manifest_path(root: &Path) -> PathBuf {
        root.join("manifest.toml")
    }

    /// Load a corpus from `root`, reading `root/manifest.toml`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::manifest_path(root);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read manifest at {}", path.display()))?;
        let manifest = Manifest::from_toml(&text)?;
        Ok(Self {
            root: root.to_path_buf(),
            manifest,
        })
    }

    /// Path to a case/variant audio file, `<root>/<case-id>/<variant>.wav`.
    pub fn audio_path(&self, case_id: &str, variant: &str) -> PathBuf {
        self.root.join(case_id).join(format!("{variant}.wav"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_minimal_manifest() {
        let toml = r#"
[[case]]
id = "punct-basic-de"
spoken = "Guten Tag Punkt hallo Komma wie geht es dir Fragezeichen"
expected = "Guten Tag. Hallo, wie geht es dir?"
tags = ["punctuation", "de"]
variants = ["normal", "fast", "noise"]
"#;
        let m = Manifest::from_toml(toml).unwrap();
        assert_eq!(m.cases.len(), 1);
        let c = &m.cases[0];
        assert_eq!(c.id, "punct-basic-de");
        assert_eq!(c.expected, "Guten Tag. Hallo, wie geht es dir?");
        assert_eq!(c.variants, vec!["normal", "fast", "noise"]);
    }

    #[test]
    fn variants_default_when_omitted() {
        let m = Manifest::from_toml("[[case]]\nid = \"x\"\nspoken = \"hallo\"\n").unwrap();
        assert_eq!(m.cases[0].variants, default_variants());
        assert_eq!(m.cases[0].expected, "");
    }

    /// The committed corpus manifest must always parse and be internally sound:
    /// unique ids, non-empty spoken/expected, and at least one variant each.
    #[test]
    fn shipped_manifest_is_valid() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../bench/corpus/manifest.toml")
            .canonicalize()
            .expect("shipped manifest should exist");
        let text = std::fs::read_to_string(&path).unwrap();
        let m = Manifest::from_toml(&text).unwrap();
        assert!(m.cases.len() >= 15, "expected the shipped ~15 cases");

        let mut ids = std::collections::HashSet::new();
        for c in &m.cases {
            assert!(ids.insert(c.id.clone()), "duplicate case id: {}", c.id);
            assert!(!c.spoken.trim().is_empty(), "empty spoken in {}", c.id);
            assert!(!c.expected.trim().is_empty(), "empty expected in {}", c.id);
            assert!(!c.variants.is_empty(), "no variants in {}", c.id);
        }
    }
}
