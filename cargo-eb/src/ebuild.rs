//! Ebuild generation and `--update` rewriting of the generated parts.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::Datelike;
use minijinja::{Environment, context};

use crate::cargo::{Crate, PackageMetadata};

const TEMPLATE: &str = include_str!("../templates/ebuild.j2");

const SOURCE_SNAPSHOT_MARKER: &str = "# Source snapshot\nSRC_URI=\"";
const CRATE_TARBALL_MARKER: &str = "# Crate tarball\nSRC_URI+=\"";
const WORKSPACE_MEMBER_MARKER: &str = "# Workspace member\nS=\"";
const CRATE_LICENSES_MARKER: &str = "# Dependent crate licenses\nLICENSE+=\"";
const CARGO_FEATURES_MARKER: &str = "# Cargo features\nIUSE+=\"";
const GIT_CRATES_START: &str = "declare -A GIT_CRATES=(";
const GENERATED_CONFIGURE_ARGS: [&str; 2] = ["--no-default-features", "--frozen"];

/// Where the dependencies come from.
pub enum Deps<'a> {
    /// A vendor tarball, possibly carrying `cargo_home/sources.toml`.
    Tarball { name: &'a str, extra_sources: bool },
    /// `CRATES`/`GIT_CRATES`, with the archives fetched to `distdir`.
    Crates {
        crates: &'a [Crate],
        distdir: &'a Path,
    },
}

/// Everything the generated parts of an ebuild are computed from.
pub struct Input<'a> {
    pub pkg: &'a PackageMetadata,
    pub deps: Deps<'a>,
    pub source_tarball: Option<&'a str>,
    /// Package directory inside the snapshot, for `S`.
    pub member_dir: Option<&'a str>,
    pub mapping_path: &'a Path,
    /// `(crate, SPDX license)` of every dependency.
    pub crate_licenses: &'a [(String, String)],
}

/// The generated values, shared by [`render_ebuild`] and [`update_ebuild`].
struct Generated {
    crates: String,
    git_crates: String,
    pkg_license: String,
    crate_licenses: String,
    iuse_plus: String,
    myfeatures: String,
    configure_args: String,
}

impl Generated {
    fn new(input: &Input<'_>) -> Result<Self> {
        let (crates, git_crates) = match &input.deps {
            Deps::Tarball { .. } => (String::new(), String::new()),
            Deps::Crates { crates, distdir } => {
                (crates_var(crates), git_crates_var(crates, distdir))
            }
        };
        let needs_mapping = input.pkg.license.is_some() || !input.crate_licenses.is_empty();
        let mapping = if needs_mapping {
            crate::license::load_mapping(input.mapping_path)?
        } else {
            HashMap::new()
        };
        let pkg_license = match &input.pkg.license {
            Some(lic) => {
                let value = crate::license::spdx_to_ebuild(lic, &mapping)
                    .with_context(|| format!("license of {}", input.pkg.name))?;
                crate::license::format_license_var(&value, "LICENSE=\"")
            }
            None => String::new(),
        };
        let features = &input.pkg.features;
        let mut myfeatures: Vec<String> = features
            .iuse
            .keys()
            .map(|f| format!("\t\t$(usev {f})"))
            .collect();
        if features.no_default {
            myfeatures.extend(features.always.iter().map(|f| format!("\t\t{f}")));
        }
        Ok(Self {
            crates,
            git_crates,
            pkg_license,
            crate_licenses: crate_licenses_var(input.crate_licenses, &mapping)?,
            iuse_plus: features
                .iuse
                .iter()
                .map(|(f, default)| format!(" {}{f}", if *default { "+" } else { "" }))
                .collect(),
            myfeatures: myfeatures.join("\n"),
            configure_args: configure_args(features.no_default, &input.deps),
        })
    }
}

/// Git sources in a vendor tarball need `--frozen`, or `cargo install` re-resolves them.
fn configure_args(no_default: bool, deps: &Deps<'_>) -> String {
    let frozen = matches!(
        deps,
        Deps::Tarball {
            extra_sources: true,
            ..
        }
    );
    let flags = [(no_default, "--no-default-features"), (frozen, "--frozen")];
    flags
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, f)| format!(" {f}"))
        .collect()
}

fn crates_var(crates: &[Crate]) -> String {
    let mut entries: Vec<String> = crates.iter().filter_map(Crate::crate_entry).collect();
    if entries.is_empty() {
        return "\n".to_string();
    }
    entries.sort();
    format!("\n\t{}\n", entries.join("\n\t"))
}

/// `declare -A GIT_CRATES=(...)`, with each subdir read from the fetched archive.
fn git_crates_var(crates: &[Crate], distdir: &Path) -> String {
    let mut entries = Vec::new();
    for c in crates {
        let Crate::Git(g) = c else { continue };
        let subdir = crate::cargo::package_directory_in_archive(c, distdir).unwrap_or_else(|| {
            let repo = g.repository.rsplit('/').next().unwrap_or(&g.name);
            format!("{repo}-{}", g.commit)
        });
        if let Some(val) = c.git_crate_entry(&subdir) {
            entries.push(format!("\t[{}]='{}'", g.name, val.replace('\'', "'\\''")));
        }
    }
    if entries.is_empty() {
        return String::new();
    }
    entries.sort();
    format!("\n\n{GIT_CRATES_START}\n{}\n)", entries.join("\n"))
}

fn crate_licenses_var(
    licenses: &[(String, String)],
    mapping: &HashMap<String, String>,
) -> Result<String> {
    let mut gentoo = BTreeSet::new();
    for (krate, spdx) in licenses {
        let value = crate::license::spdx_to_ebuild(spdx, mapping)
            .with_context(|| format!("license of dependency {krate}"))?;
        gentoo.insert(value);
    }
    if gentoo.is_empty() {
        return Ok(String::new());
    }
    let combined = gentoo.into_iter().collect::<Vec<_>>().join(" ");
    let deduped = portage_metadata::LicenseExpr::parse(&combined)?
        .dedup()
        .to_string();
    let s = crate::license::format_license_var(&deduped, "LICENSE+=\" ");
    Ok(if s.starts_with('\n') {
        s
    } else {
        format!(" {s}")
    })
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bash_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
}

fn url_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(b as char),
            b'-' | b'_' | b'.' | b'~' | b'/' | b':' | b'@' | b'#' | b'?' | b'=' | b'&' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// PMS requires a non-empty `DESCRIPTION`; fall back to one built from the name.
fn pkg_description(pkg: &PackageMetadata) -> String {
    match pkg.description.as_deref().map(str::trim) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => format!("{} Rust crate", pkg.name),
    }
}

/// A new ebuild.
pub fn render_ebuild(input: &Input<'_>) -> Result<String> {
    let g = Generated::new(input)?;
    let mut env = Environment::new();
    env.set_trim_blocks(true);
    env.set_keep_trailing_newline(true);
    env.add_template("ebuild", TEMPLATE)?;
    let (crate_tarball, extra_sources) = match input.deps {
        Deps::Tarball {
            name,
            extra_sources,
        } => (name, extra_sources),
        Deps::Crates { .. } => ("", false),
    };
    let out = env.get_template("ebuild")?.render(context! {
        year => chrono::Utc::now().year(),
        prog_version => env!("CARGO_PKG_VERSION"),
        crates => g.crates,
        git_crates => g.git_crates,
        description => bash_escape(&collapse_ws(&pkg_description(input.pkg))),
        homepage => input.pkg.homepage.as_deref().filter(|s| !s.is_empty()).map(url_escape),
        crate_tarball => crate_tarball,
        extra_sources => extra_sources,
        source_tarball => input.source_tarball.unwrap_or(""),
        s_dir => input.member_dir.unwrap_or(""),
        pkg_license => g.pkg_license,
        crate_licenses => g.crate_licenses,
        iuse_plus => g.iuse_plus,
        myfeatures => g.myfeatures,
        configure_args => g.configure_args,
    })?;
    Ok(out)
}

/// Replace `marker` and its quoted value up to the closing `"`.
fn replace_quoted(hay: &mut String, marker: &str, value: &str) -> Result<()> {
    let start = hay
        .find(marker)
        .with_context(|| format!("missing generated marker {marker:?}"))?;
    let after = start + marker.len();
    let rel = hay[after..]
        .find('"')
        .with_context(|| format!("unclosed quote after {marker:?}"))?;
    hay.replace_range(start..after + rel + 1, &format!("{marker}{value}\""));
    Ok(())
}

/// Byte offset of the start of the first line after `from` that is `line` once trimmed.
fn find_line(hay: &str, from: usize, line: &str) -> Option<usize> {
    let mut pos = from;
    for l in hay[from..].split_inclusive('\n') {
        if l.trim() == line {
            return Some(pos);
        }
        pos += l.len();
    }
    None
}

/// Rewrite the `local myfeatures=(...)` body.
fn replace_myfeatures(hay: &mut String, myfeatures: &str) -> Result<()> {
    const START: &str = "local myfeatures=(\n";
    let open = hay.find(START).context("missing `local myfeatures=(`")? + START.len();
    let close = find_line(hay, open, ")").context("unclosed `local myfeatures=(`")?;
    let body = if myfeatures.is_empty() {
        String::new()
    } else {
        format!("{myfeatures}\n")
    };
    hay.replace_range(open..close, &body);
    Ok(())
}

/// Rewrite the `cargo_src_configure` call carrying only generated flags; false if none.
fn replace_configure_call(hay: &mut String, args: &str) -> bool {
    let mut pos = 0;
    for line in hay.split_inclusive('\n') {
        let mut words = line.split_whitespace();
        if words.next() == Some("cargo_src_configure")
            && words.all(|w| GENERATED_CONFIGURE_ARGS.contains(&w))
        {
            let indent = line.len() - line.trim_start().len();
            let end = pos + line.trim_end().len();
            hay.replace_range(pos + indent..end, &format!("cargo_src_configure{args}"));
            return true;
        }
        pos += line.len();
    }
    false
}

/// Rewrite the generated parts of `existing`; maintainer `IUSE=`/`LICENSE=` stay.
pub fn update_ebuild(existing: &str, input: &Input<'_>) -> Result<String> {
    let g = Generated::new(input)?;
    let mut out = existing.to_string();

    match input.source_tarball {
        Some(src) => replace_quoted(&mut out, SOURCE_SNAPSHOT_MARKER, src)?,
        None if out.contains(SOURCE_SNAPSHOT_MARKER) => {
            tracing::warn!(
                "the crate is a release now; the source snapshot SRC_URI was left as is"
            );
        }
        None => {}
    }

    if let Some(dir) = input.member_dir {
        replace_quoted(
            &mut out,
            WORKSPACE_MEMBER_MARKER,
            &format!("${{WORKDIR}}/${{P}}/{dir}"),
        )?;
    }

    match input.deps {
        Deps::Tarball {
            name,
            extra_sources,
        } => {
            replace_quoted(&mut out, CRATE_TARBALL_MARKER, &format!(" {name}"))?;
            if extra_sources && !out.contains("sources.toml") {
                bail!(
                    "the vendor tarball now carries git sources; add a src_unpack that appends \
                     ${{ECARGO_HOME}}/sources.toml to ${{ECARGO_HOME}}/config.toml"
                );
            }
        }
        Deps::Crates { .. } => update_crates(&mut out, &g)?,
    }

    if out.contains(CRATE_LICENSES_MARKER) {
        replace_quoted(&mut out, CRATE_LICENSES_MARKER, &g.crate_licenses)?;
    } else if !g.crate_licenses.is_empty() {
        let lic = out.find("\nLICENSE=\"").context("missing LICENSE=")? + "\nLICENSE=\"".len();
        let end = lic + out[lic..].find('"').context("unclosed LICENSE=")? + 1;
        out.insert_str(
            end,
            &format!("\n{CRATE_LICENSES_MARKER}{}\"", g.crate_licenses),
        );
    }

    if !g.iuse_plus.is_empty() || out.contains(CARGO_FEATURES_MARKER) {
        replace_quoted(&mut out, CARGO_FEATURES_MARKER, &g.iuse_plus)?;
        replace_myfeatures(&mut out, &g.myfeatures)?;
    }
    if !replace_configure_call(&mut out, &g.configure_args) && !g.configure_args.is_empty() {
        bail!(
            "no `cargo_src_configure` call to pass{} to",
            g.configure_args
        );
    }

    Ok(out)
}

fn update_crates(out: &mut String, g: &Generated) -> Result<()> {
    let crates = out.find("\nCRATES=\"").context("CRATES= not found")?;
    if out
        .find("\ninherit ")
        .is_some_and(|inherit| inherit < crates)
    {
        bail!("CRATES= must come before `inherit cargo`, move it up and re-run");
    }
    replace_quoted(out, "\nCRATES=\"", &g.crates)?;
    match out.find(GIT_CRATES_START) {
        Some(start) => {
            let close = find_line(out, start, ")").context("unclosed GIT_CRATES")?;
            let end = close + out[close..].find(')').unwrap_or(0) + 1;
            if g.git_crates.is_empty() {
                let start = out[..start].trim_end_matches('\n').len();
                out.replace_range(start..end, "");
            } else {
                out.replace_range(start..end, g.git_crates.trim_start());
            }
        }
        None if !g.git_crates.is_empty() => {
            let start = out.find("\nCRATES=\"").unwrap_or(0) + "\nCRATES=\"".len();
            let end = start + out[start..].find('"').unwrap_or(0) + 1;
            out.insert_str(end, &g.git_crates);
        }
        None => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cargo::{Features, FileCrate};
    use std::collections::BTreeMap;

    fn pkg(description: Option<&str>) -> PackageMetadata {
        PackageMetadata {
            name: "foo".to_string(),
            version: "1.0.0".to_string(),
            license: None,
            description: description.map(str::to_string),
            homepage: None,
            features: Features::default(),
            publish: true,
        }
    }

    fn pkg_with_features() -> PackageMetadata {
        let mut p = pkg(Some("does a thing"));
        p.features = Features {
            iuse: BTreeMap::from([("serde".into(), true), ("json".into(), false)]),
            always: vec!["dep/std".into()],
            no_default: true,
        };
        p
    }

    fn input<'a>(pkg: &'a PackageMetadata, deps: Deps<'a>) -> Input<'a> {
        Input {
            pkg,
            deps,
            source_tarball: None,
            member_dir: None,
            mapping_path: Path::new("/nonexistent"),
            crate_licenses: &[],
        }
    }

    fn no_crates() -> Deps<'static> {
        Deps::Crates {
            crates: &[],
            distdir: Path::new("/nonexistent"),
        }
    }

    #[test]
    fn pkg_description_falls_back_when_missing() {
        assert_eq!(pkg_description(&pkg(None)), "foo Rust crate");
        assert_eq!(pkg_description(&pkg(Some("  "))), "foo Rust crate");
        assert_eq!(pkg_description(&pkg(Some("does a thing"))), "does a thing");
    }

    #[test]
    fn crates_precede_inherit_and_keep_snapshot_src_uri() {
        let crates = [Crate::File(FileCrate {
            name: "dep".into(),
            version: "1.0.0".into(),
            checksum: String::new(),
        })];
        let p = pkg(None);
        let mut i = input(
            &p,
            Deps::Crates {
                crates: &crates,
                distdir: Path::new("/nonexistent"),
            },
        );
        i.source_tarball = Some("foo-1.0.0.tar.xz");
        let out = render_ebuild(&i).unwrap();
        let crates_at = out.find("CRATES=\"\n\tdep@1.0.0\n\"").expect(&out);
        assert!(crates_at < out.find("inherit cargo").unwrap(), "{out}");
        assert!(
            out.contains("SRC_URI=\"foo-1.0.0.tar.xz\"\nSRC_URI+=\" ${CARGO_CRATE_URIS}\""),
            "{out}"
        );
    }

    #[test]
    fn render_tarball_workspace_member() {
        let p = pkg_with_features();
        let mut i = input(
            &p,
            Deps::Tarball {
                name: "foo-1.0.0-crates.tar.xz",
                extra_sources: true,
            },
        );
        i.source_tarball = Some("foo-1.0.0.tar.xz");
        i.member_dir = Some("crates/foo");
        let out = render_ebuild(&i).unwrap();
        assert!(
            out.contains("# Crate tarball\nSRC_URI+=\" foo-1.0.0-crates.tar.xz\""),
            "{out}"
        );
        assert!(out.contains("S=\"${WORKDIR}/${P}/crates/foo\""), "{out}");
        assert!(
            out.contains("sources.toml\" >> \"${ECARGO_HOME}/config.toml\""),
            "{out}"
        );
        assert!(out.contains("\t\tdep/std\n"), "{out}");
        assert!(
            out.contains("cargo_src_configure --no-default-features --frozen\n"),
            "{out}"
        );
        assert!(!out.contains("CRATES="), "{out}");
        assert!(!out.contains("HOMEPAGE="), "{out}");
    }

    #[test]
    fn update_rewrites_myfeatures_body_and_keeps_maintainer_iuse() {
        let existing = "EAPI=8\n\nCRATES=\"\n\"\n\ninherit cargo\n\nIUSE=\"debug\"\n# Cargo features\nIUSE+=\" old\"\n\n\
                        src_configure() {\n\tlocal myfeatures=(\n\t\t$(usev old)\n\t)\n\tcargo_src_configure\n}\n";
        let p = pkg_with_features();
        let out = update_ebuild(existing, &input(&p, no_crates())).unwrap();
        let expected = "IUSE=\"debug\"\n# Cargo features\nIUSE+=\" json +serde\"\n\nsrc_configure() {\n\
                        \tlocal myfeatures=(\n\t\t$(usev json)\n\t\t$(usev serde)\n\t\tdep/std\n\t)\n\
                        \tcargo_src_configure --no-default-features\n}\n";
        assert!(out.ends_with(expected), "{out}");
        let again = update_ebuild(&out, &input(&p, no_crates())).unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn update_rejects_crates_after_inherit() {
        let existing = "EAPI=8\n\ninherit cargo\n\nCRATES=\"\n\"\n";
        let err = update_ebuild(existing, &input(&pkg(None), no_crates())).unwrap_err();
        assert!(err.to_string().contains("before `inherit cargo`"), "{err}");
    }

    #[test]
    fn update_without_feature_marker_errors() {
        let existing = "\nCRATES=\"\n\"\nIUSE=\"debug\"\n";
        let err = update_ebuild(existing, &input(&pkg_with_features(), no_crates())).unwrap_err();
        assert!(err.to_string().contains("Cargo features"), "{err}");
    }
}
