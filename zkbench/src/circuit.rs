//! Circuit discovery: validate the directory and derive artifact paths.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// A resolved, validated circuit under test.
///
/// Artifact filenames all derive from the package `name` in Nargo.toml:
///   target/<name>.json  = ACIR bytecode  (input to `bb`)
///   target/<name>.gz    = witness        (input to `bb prove`)
#[derive(Debug)]
pub struct Circuit {
    pub name: String,
    pub dir: PathBuf,
    pub bytecode: PathBuf,
    pub witness: PathBuf,
    pub proof_dir: PathBuf,
    /// Directory for the precomputed verification key. `bb write_vk -o <vk_dir>`
    /// writes a `vk` file inside it, which `bb prove -k` then reads.
    pub vk_dir: PathBuf,
}

/// Minimal view of Nargo.toml — we only need the package name.
#[derive(Deserialize)]
struct NargoToml {
    package: PackageSection,
}

#[derive(Deserialize)]
struct PackageSection {
    name: String,
}

impl Circuit {
    /// Validate `dir` is a Noir circuit folder and compute artifact paths.
    /// Errors here are the common "wrong folder" mistakes, phrased for the user.
    pub async fn load(dir: &Path) -> Result<Circuit> {
        let dir = tokio::fs::canonicalize(dir)
            .await
            .with_context(|| format!("path not found: {}", dir.display()))?;

        let nargo_toml = dir.join("Nargo.toml");
        if !tokio::fs::try_exists(&nargo_toml).await.unwrap_or(false) {
            bail!("this does not look like a circuit folder (no Nargo.toml): {}", dir.display());
        }
        let prover_toml = dir.join("Prover.toml");
        if !tokio::fs::try_exists(&prover_toml).await.unwrap_or(false) {
            bail!("missing input file (Prover.toml): {}", prover_toml.display());
        }

        let text = tokio::fs::read_to_string(&nargo_toml)
            .await
            .with_context(|| format!("failed to read Nargo.toml: {}", nargo_toml.display()))?;
        let parsed: NargoToml =
            toml::from_str(&text).context("failed to parse Nargo.toml (check package.name)")?;
        let name = parsed.package.name;

        let target = dir.join("target");
        Ok(Circuit {
            bytecode: target.join(format!("{name}.json")),
            witness: target.join(format!("{name}.gz")),
            proof_dir: target.join("proof"),
            vk_dir: target.join("vk"),
            name,
            dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_nargo_toml(dir: &Path, name: &str) {
        fs::write(
            dir.join("Nargo.toml"),
            format!("[package]\nname = \"{name}\"\ntype = \"bin\"\n"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn missing_nargo_toml_is_rejected() {
        let dir = tempdir().unwrap();
        let err = Circuit::load(dir.path()).await.unwrap_err();
        assert!(err.to_string().contains("Nargo.toml"), "got: {err}");
    }

    #[tokio::test]
    async fn missing_prover_toml_is_rejected() {
        let dir = tempdir().unwrap();
        write_nargo_toml(dir.path(), "demo");
        let err = Circuit::load(dir.path()).await.unwrap_err();
        assert!(err.to_string().contains("Prover.toml"), "got: {err}");
    }

    #[tokio::test]
    async fn valid_circuit_parses_name_and_derives_paths() {
        let dir = tempdir().unwrap();
        write_nargo_toml(dir.path(), "demo");
        fs::write(dir.path().join("Prover.toml"), "").unwrap();

        let circuit = Circuit::load(dir.path()).await.unwrap();
        assert_eq!(circuit.name, "demo");
        assert!(circuit.bytecode.ends_with("target/demo.json"));
        assert!(circuit.witness.ends_with("target/demo.gz"));
        assert!(circuit.proof_dir.ends_with("target/proof"));
    }
}
