//! `--setup` milestones: which bootstrap steps a target has completed, so `--setup -r` can
//! continue where an earlier run stopped instead of rebuilding every step
//!
//! The record lives in the sysroot. A step counts as done when it was recorded with the same
//! spec, atoms and USE, and what it merged is still installed; the first step that does not
//! hold runs again, and every step after it.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Dep;
use portage_vdb::Vdb;
use serde::{Deserialize, Serialize};

use super::stages::StageStep;

/// The record's location under a sysroot
const RECORD: &str = "var/lib/em/crossdev-setup.json";

/// The completed steps of one target's bootstrap
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Milestones {
    /// The spec the steps were built from, as TOML
    spec: String,
    /// Completed steps, in plan order
    steps: Vec<Milestone>,
}

/// One completed step
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Milestone {
    label: String,
    atoms: Vec<String>,
    use_override: Vec<String>,
    /// The installed packages matching the step's atoms once it finished
    installed: Vec<String>,
}

impl Milestones {
    fn path(sysroot: &Utf8Path) -> Utf8PathBuf {
        sysroot.join(RECORD)
    }

    /// The record for `sysroot` if it was made with `spec`
    pub(crate) fn load(sysroot: &Utf8Path, spec: &str) -> Option<Self> {
        let text = std::fs::read_to_string(Self::path(sysroot)).ok()?;
        serde_json::from_str::<Self>(&text)
            .ok()
            .filter(|m| m.spec == spec)
    }

    /// How many leading `steps` are still done: recorded alike, with what they merged
    /// still installed under the VDB root `vdb_root` gives for each step
    pub(crate) fn done(
        &self,
        steps: &[StageStep],
        vdb_root: impl Fn(&StageStep) -> Utf8PathBuf,
    ) -> usize {
        steps
            .iter()
            .zip(&self.steps)
            .take_while(|(step, m)| {
                m.label == step.label
                    && m.atoms == step.atoms
                    && m.use_override == step.use_override
                    && installed(&vdb_root(step), &step.atoms) == m.installed
            })
            .count()
    }

    /// Record `steps[index]` as completed, dropping anything recorded after it
    pub(crate) fn complete(
        sysroot: &Utf8Path,
        spec: &str,
        steps: &[StageStep],
        index: usize,
        vdb_root: &Utf8Path,
    ) -> Result<()> {
        let mut record = Self::load(sysroot, spec).unwrap_or_else(|| Self {
            spec: spec.to_owned(),
            steps: Vec::new(),
        });
        record.steps.truncate(index);
        let step = &steps[index];
        record.steps.push(Milestone {
            label: step.label.clone(),
            atoms: step.atoms.clone(),
            use_override: step.use_override.clone(),
            installed: installed(vdb_root, &step.atoms),
        });
        let path = Self::path(sysroot);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
        }
        let text = serde_json::to_string_pretty(&record).context("serializing the milestones")?;
        std::fs::write(&path, text).with_context(|| format!("writing {path}"))
    }

    /// Forget every recorded step, for a run that rebuilds them all
    pub(crate) fn clear(sysroot: &Utf8Path) -> Result<()> {
        match std::fs::remove_file(Self::path(sysroot)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", Self::path(sysroot)))
            }
            _ => Ok(()),
        }
    }
}

/// The installed CPVs under `vdb_root` matching any of `atoms`, sorted
fn installed(vdb_root: &Utf8Path, atoms: &[String]) -> Vec<String> {
    let deps: Vec<Dep> = atoms.iter().filter_map(|a| Dep::parse(a).ok()).collect();
    let Ok(vdb) = Vdb::open(vdb_root.join("var/db/pkg")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = vdb
        .packages()
        .into_iter()
        .filter(|pkg| {
            deps.iter()
                .any(|dep| dep.matches_cpv(pkg.cpv(), pkg.slot().ok().as_ref()))
        })
        .map(|pkg| pkg.cpv().to_string())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crossdev::stages::Level;

    fn step(label: &str, atom: &str) -> StageStep {
        StageStep {
            label: label.into(),
            atoms: vec![atom.into()],
            use_override: vec![],
            nodeps: false,
            into_sysroot: false,
            level: Level::Binutils,
        }
    }

    fn install(root: &Utf8Path, cpv: &str) {
        let dir = root.join("var/db/pkg").join(cpv);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, value) in [("EAPI", "8"), ("SLOT", "0"), ("CONTENTS", "")] {
            std::fs::write(dir.join(file), value).unwrap();
        }
    }

    #[test]
    fn steps_stay_done_while_spec_steps_and_installs_hold() {
        let dir = tempfile::tempdir().unwrap();
        let sysroot = Utf8Path::from_path(dir.path()).unwrap().join("sysroot");
        let host = Utf8Path::from_path(dir.path()).unwrap().join("host");
        let steps = [
            step("binutils", "cross-t/binutils"),
            step("libc", "cross-t/glibc"),
        ];
        install(&host, "cross-t/binutils-2.47");
        install(&host, "cross-t/glibc-2.44");
        Milestones::complete(&sysroot, "spec", &steps, 0, &host).unwrap();
        Milestones::complete(&sysroot, "spec", &steps, 1, &host).unwrap();
        let roots = |_: &StageStep| host.clone();

        assert_eq!(
            Milestones::load(&sysroot, "spec")
                .unwrap()
                .done(&steps, roots),
            2
        );
        assert!(Milestones::load(&sysroot, "other spec").is_none());

        // An upgraded package means its step and everything after it run again.
        std::fs::rename(
            host.join("var/db/pkg/cross-t/binutils-2.47"),
            host.join("var/db/pkg/cross-t/binutils-2.48"),
        )
        .unwrap();
        assert_eq!(
            Milestones::load(&sysroot, "spec")
                .unwrap()
                .done(&steps, roots),
            0
        );

        Milestones::clear(&sysroot).unwrap();
        assert!(Milestones::load(&sysroot, "spec").is_none());
    }
}
