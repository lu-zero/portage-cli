//! `em portageq` — Portage's own query interface (subset)
//!
//! VDB, settings and repos groups work; metadata/contents/owners,
//! best_visible and protect/eclass/license groups do not exist yet.
//!
//! Eroot-taking commands read host global defaults plus the eroot's own
//! user config; config-only commands use `em`'s own roots.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::{Cpv, Dep};
use portage_vdb::{InstalledPackage, Vdb};

/// Real portageq's exit code when `<EROOT>` doesn't exist as a directory.
const EROOT_NOT_A_DIRECTORY: u8 = 64;
/// Real portageq's exit code for an atom that doesn't parse.
const INVALID_ATOM: u8 = 2;

/// Validate `<EROOT>` per real portageq: it must already exist as a
/// directory (an empty/not-yet-bootstrapped root is fine — only a
/// genuinely missing path is rejected here).
fn validate_eroot(eroot: &str) -> std::result::Result<(), u8> {
    if std::path::Path::new(eroot).is_dir() {
        Ok(())
    } else {
        eprintln!("Not a directory: '{eroot}'");
        eprintln!("Run portageq with --help for info");
        Err(EROOT_NOT_A_DIRECTORY)
    }
}

/// Every installed package under `<EROOT>/var/db/pkg`, or empty when that
/// directory doesn't exist yet (a fresh root with nothing installed is a
/// normal case, not an error — `<EROOT>` itself was already validated).
fn installed_packages(eroot: &str) -> Vec<InstalledPackage> {
    let vdb_path = format!("{}/var/db/pkg", eroot.trim_end_matches('/'));
    match Vdb::open(vdb_path.as_str()) {
        Ok(vdb) => vdb.packages().into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

fn parse_atom(atom: &str) -> std::result::Result<Dep, u8> {
    Dep::parse(atom).map_err(|_| {
        eprintln!("ERROR: Invalid atom: '{atom}'");
        INVALID_ATOM
    })
}

fn matches(dep: &Dep, pkg: &InstalledPackage) -> bool {
    dep.matches_cpv(pkg.cpv(), pkg.slot().ok().as_ref())
}

pub(crate) fn has_version(eroot: &str, atom: &str) -> Result<u8> {
    if let Err(code) = validate_eroot(eroot) {
        return Ok(code);
    }
    let dep = match parse_atom(atom) {
        Ok(d) => d,
        Err(code) => return Ok(code),
    };
    let pkgs = installed_packages(eroot);
    Ok(if pkgs.iter().any(|p| matches(&dep, p)) {
        0
    } else {
        1
    })
}

pub(crate) fn best_version(eroot: &str, atom: &str) -> Result<u8> {
    if let Err(code) = validate_eroot(eroot) {
        return Ok(code);
    }
    let dep = match parse_atom(atom) {
        Ok(d) => d,
        Err(code) => return Ok(code),
    };
    let pkgs = installed_packages(eroot);
    let best = pkgs
        .iter()
        .filter(|p| matches(&dep, p))
        .map(|p| p.cpv())
        .max();
    match best {
        Some(cpv) => println!("{cpv}"),
        None => println!(),
    }
    Ok(0)
}

/// `atom` empty matches every installed package. A non-empty atom that
/// isn't a plain dependency atom (e.g. `qgrep`-style `cat/*` glob syntax)
/// is not supported.
pub(crate) fn run_match(eroot: &str, atom: &str) -> Result<u8> {
    if let Err(code) = validate_eroot(eroot) {
        return Ok(code);
    }
    let pkgs = installed_packages(eroot);
    let mut cpvs: Vec<Cpv> = if atom.is_empty() {
        pkgs.iter().map(|p| p.cpv().clone()).collect()
    } else {
        let dep = match parse_atom(atom) {
            Ok(d) => d,
            Err(code) => return Ok(code),
        };
        pkgs.iter()
            .filter(|p| matches(&dep, p))
            .map(|p| p.cpv().clone())
            .collect()
    };
    cpvs.sort();
    for cpv in &cpvs {
        println!("{cpv}");
    }
    Ok(0)
}

/// Missing operands, reported the way portageq does.
fn check_arity(names: &[String], out: &mut dyn std::io::Write) -> Result<Option<u8>> {
    if names.is_empty() {
        writeln!(out, "ERROR: insufficient parameters!")?;
        Ok(Some(2))
    } else {
        Ok(None)
    }
}

/// `envvar -v` line: `NAME=value` bare when safe, else single-quoted.
pub(crate) fn format_verbose(name: &str, value: &str) -> String {
    if value.is_empty() {
        return format!("{name}=''");
    }
    let safe = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"@%_+=:,./-".contains(&b));
    if safe {
        format!("{name}={value}")
    } else {
        format!("{name}='{}'", value.replace('\'', "'\"'\"'"))
    }
}

/// Writes one line per name: the value or an empty line when unset.
pub(crate) fn write_values(
    names: &[String],
    verbose: bool,
    values: &[Option<String>],
    out: &mut dyn std::io::Write,
) -> Result<u8> {
    let mut code = 0;
    for (name, value) in names.iter().zip(values.iter()) {
        match value {
            Some(v) => {
                if verbose {
                    writeln!(out, "{}", format_verbose(name, v))?;
                } else {
                    writeln!(out, "{v}")?;
                }
            }
            None => {
                writeln!(out)?;
                code = 1;
            }
        }
    }
    Ok(code)
}

/// Profile shell with the root's config already applied.
async fn profile_shell(cli: &crate::cli::Cli) -> Result<portage_repo::EbuildShell> {
    let roots = cli.roots();
    let repo = crate::crossdev::main_repo(cli)?;
    let mut shell = repo.shell().await.context("creating shell")?;
    crate::ebuild::apply_profile_env(&mut shell, roots.config(), roots.config_overlay()).await?;
    Ok(shell)
}

/// Full `envvar` path: profile shell lookup, then shared rendering.
///
/// Ambient environment wins over the shell, set-but-empty included.
pub(crate) async fn run_envvar(
    cli: &crate::cli::Cli,
    names: &[String],
    verbose: bool,
    out: &mut dyn std::io::Write,
) -> Result<u8> {
    if let Some(code) = check_arity(names, out)? {
        return Ok(code);
    }
    let shell = profile_shell(cli).await?;
    let values: Vec<Option<String>> = names
        .iter()
        .map(|n| {
            std::env::var(n)
                .ok()
                .or_else(|| crate::info::resolve_info_var(&shell, n))
        })
        .collect();
    write_values(names, verbose, &values, out)
}

/// Single profile value on one line; unset prints empty and exits 1.
pub(crate) async fn run_settings_var(
    cli: &crate::cli::Cli,
    var: &str,
    out: &mut dyn std::io::Write,
) -> Result<u8> {
    if var == "CONFIG_PROTECT" || var == "CONFIG_PROTECT_MASK" {
        let lists = crate::ebuild::protect::ConfigProtect::from_roots(&cli.roots()).await;
        let mut dirs: Vec<String> = if var == "CONFIG_PROTECT" {
            lists.protected_dirs().to_vec()
        } else {
            lists.masked_dirs().to_vec()
        };
        // Verified live: real portageq prints the accumulated list sorted.
        dirs.sort();
        writeln!(out, "{}", dirs.join(" "))?;
        return Ok(0);
    }
    let shell = profile_shell(cli).await?;
    match crate::info::resolve_info_var(&shell, var) {
        Some(v) => {
            writeln!(out, "{v}")?;
            Ok(0)
        }
        None => {
            writeln!(out)?;
            Ok(1)
        }
    }
}

/// `<merge_root>/var/db/pkg`, no shell involved.
pub(crate) fn vdb_path(cli: &crate::cli::Cli, out: &mut dyn std::io::Write) -> Result<u8> {
    writeln!(out, "{}", cli.roots().merge_root().join("var/db/pkg"))?;
    Ok(0)
}

/// Legacy path plus its deprecation warning on stderr.
pub(crate) fn portdir(
    cli: &crate::cli::Cli,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> Result<u8> {
    writeln!(out, "{}", cli.repo_path())?;
    writeln!(
        err,
        "WARNING: 'portageq portdir' is deprecated. Use the get_repo_path command instead. \
         eg: 'portageq get_repo_path / gentoo' instead."
    )?;
    Ok(0)
}

/// Legacy overlays: `PORTDIR_OVERLAY` when set, else non-main repos.
pub(crate) fn portdir_overlay(
    cli: &crate::cli::Cli,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> Result<u8> {
    let roots = cli.roots();
    let mut overlays: Vec<String> = roots
        .portdir_overlay()
        .iter()
        .map(|p| p.to_string())
        .collect();
    if overlays.is_empty()
        && let Ok(conf) = eroot_repos_conf(&roots)
    {
        overlays = fallback_overlays(&conf);
    }
    writeln!(out, "{}", overlays.join(" "))?;
    writeln!(
        err,
        "WARNING: 'portageq portdir_overlay' is deprecated. Use the get_repos and get_repo_path \
         commands or the repos_config command instead. eg: 'portageq repos_config /'"
    )?;
    Ok(0)
}

/// Non-main repo locations for the legacy overlay query.
fn fallback_overlays(conf: &portage_repo::ReposConf) -> Vec<String> {
    let main = conf.main_repo().map(|r| &r.name);
    conf.repos()
        .iter()
        .filter(|r| Some(&r.name) != main)
        .filter_map(repo_location)
        .collect()
}

/// Printable repo location, lossy only for non-UTF-8 paths.
fn repo_location(entry: &portage_repo::RepoEntry) -> Option<String> {
    let path = entry.location.as_path()?;
    Some(
        Utf8Path::from_path(path)
            .map(|p| p.to_string())
            .unwrap_or_else(|| path.to_string_lossy().into_owned()),
    )
}

/// Repos for an eroot-taking command: host global defaults plus the
/// eroot's own user config, the combination real portageq reads.
fn eroot_repos_conf(roots: &portage_resolve::Roots) -> Result<portage_repo::ReposConf> {
    let cfg = roots.config().unwrap_or(Utf8Path::new("/"));
    repos_conf_from(
        Utf8Path::new("/usr/share/portage/config/repos.conf"),
        Some(&cfg.join("etc/portage/repos.conf")),
        &roots.portdir_overlay(),
    )
}

/// Repos from explicit global and user paths; missing paths are skipped.
fn repos_conf_from(
    global: &Utf8Path,
    user: Option<&Utf8PathBuf>,
    overlays: &[Utf8PathBuf],
) -> Result<portage_repo::ReposConf> {
    let mut paths = vec![global.to_path_buf()];
    if let Some(u) = user {
        paths.push(u.clone());
    }
    let conf = portage_repo::ReposConf::load_from(&paths)?;
    Ok(conf.with_portdir_overlay(overlays))
}

/// Portage repo-name rule: alphanumerics, `-` and `_` only.
pub(crate) fn is_valid_repo_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Config [`Roots`] for an eroot-taking command: `<EROOT>` must exist as a
/// directory, then serves as its own config/base/target with host BROOT.
fn conf_roots(
    eroot: &str,
    err: &mut dyn std::io::Write,
) -> std::result::Result<portage_resolve::Roots, u8> {
    if std::path::Path::new(eroot).is_dir() {
        let root = Utf8PathBuf::from(eroot);
        Ok(portage_resolve::Roots::default()
            .with_config(Some(root.clone()))
            .with_base(Some(root.clone()))
            .with_target(Some(root))
            .with_broot(Some(Utf8PathBuf::from("/"))))
    } else {
        writeln!(err, "Not a directory: '{eroot}'").map_err(|_| 64)?;
        writeln!(err, "Run portageq with --help for info").map_err(|_| 64)?;
        Err(64)
    }
}

/// Space-separated repo names in reverse resolution order (overlays first).
pub(crate) fn get_repos(
    eroot: &str,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> Result<u8> {
    let roots = match conf_roots(eroot, err) {
        Ok(r) => r,
        Err(code) => return Ok(code),
    };
    let conf = eroot_repos_conf(&roots)?;
    let names: Vec<String> = conf
        .repos()
        .iter()
        .rev()
        .map(|r| r.name.to_string())
        .collect();
    writeln!(out, "{}", names.join(" "))?;
    Ok(0)
}

/// One location per repo; unknown prints an empty line and stops (exit 1),
/// an invalid name errors at once (exit 2).
pub(crate) fn get_repo_path(
    eroot: &str,
    repos: &[String],
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> Result<u8> {
    let roots = match conf_roots(eroot, err) {
        Ok(r) => r,
        Err(code) => return Ok(code),
    };
    if repos.is_empty() {
        writeln!(err, "ERROR: insufficient parameters!")?;
        return Ok(3);
    }
    let conf = eroot_repos_conf(&roots)?;
    let mut code = 0;
    for name in repos {
        if !is_valid_repo_name(name) {
            writeln!(err, "ERROR: invalid repository: {name}")?;
            return Ok(2);
        }
        match conf.find(name).and_then(|e| e.location.as_path()) {
            Some(path) => writeln!(out, "{}", path.display())?,
            None => {
                writeln!(out)?;
                code = 1;
                break;
            }
        }
    }
    Ok(code)
}

pub(crate) fn mass_best_version(eroot: &str, atoms: &[String]) -> Result<u8> {
    if let Err(code) = validate_eroot(eroot) {
        return Ok(code);
    }
    let pkgs = installed_packages(eroot);
    for atom in atoms {
        let best = Dep::parse(atom).ok().and_then(|dep| {
            pkgs.iter()
                .filter(|p| matches(&dep, p))
                .map(|p| p.cpv())
                .max()
        });
        match best {
            Some(cpv) => println!("{atom}:{cpv}"),
            None => println!("{atom}:"),
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway VDB tree: `<tmp>/var/db/pkg/<cat>/<pf>/` with the
    /// minimal files `InstalledPackage`/`slot()` need.
    struct TestVdb {
        _tmp: tempfile::TempDir,
        eroot: String,
    }

    impl TestVdb {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            Self {
                eroot: tmp.path().to_string_lossy().into_owned(),
                _tmp: tmp,
            }
        }

        fn add(&self, cpv: &str, slot: &str) {
            let cpv_parsed = Cpv::parse(cpv).unwrap();
            let pf = format!("{}-{}", cpv_parsed.cpn.package, cpv_parsed.version);
            let dir = std::path::Path::new(&self.eroot)
                .join("var/db/pkg")
                .join(cpv_parsed.cpn.category.as_str())
                .join(&pf);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SLOT"), format!("{slot}\n")).unwrap();
        }
    }

    #[test]
    fn has_version_hit_and_miss() {
        let vdb = TestVdb::new();
        vdb.add("dev-python/setuptools-83.0.0", "0");
        assert_eq!(has_version(&vdb.eroot, "dev-python/setuptools").unwrap(), 0);
        assert_eq!(has_version(&vdb.eroot, "dev-python/pip").unwrap(), 1);
    }

    #[test]
    fn has_version_invalid_atom() {
        let vdb = TestVdb::new();
        assert_eq!(has_version(&vdb.eroot, "not an atom!!").unwrap(), 2);
    }

    #[test]
    fn has_version_missing_eroot() {
        assert_eq!(
            has_version("/no/such/eroot", "dev-lang/python").unwrap(),
            64
        );
    }

    #[test]
    fn best_version_hit_exits_zero() {
        // best_version prints via println!, which these fast unit tests
        // don't capture — cpv_ordering_picks_the_highest_version below
        // exercises the actual max-by-version selection directly.
        let vdb = TestVdb::new();
        vdb.add("dev-python/setuptools-79.0.1", "0");
        vdb.add("dev-python/setuptools-83.0.0", "0");
        assert_eq!(
            best_version(&vdb.eroot, "dev-python/setuptools").unwrap(),
            0
        );
    }

    #[test]
    fn best_version_no_match_still_exits_zero() {
        let vdb = TestVdb::new();
        assert_eq!(
            best_version(&vdb.eroot, "dev-python/setuptools").unwrap(),
            0
        );
    }

    #[test]
    fn run_match_empty_atom_matches_everything() {
        let vdb = TestVdb::new();
        vdb.add("dev-python/setuptools-83.0.0", "0");
        vdb.add("dev-python/pip-26.1.2", "0");
        assert_eq!(run_match(&vdb.eroot, "").unwrap(), 0);
    }

    #[test]
    fn run_match_invalid_atom_is_rejected() {
        let vdb = TestVdb::new();
        assert_eq!(run_match(&vdb.eroot, "not an atom!!").unwrap(), 2);
    }

    #[test]
    fn mass_best_version_mixes_hits_and_misses() {
        let vdb = TestVdb::new();
        vdb.add("dev-python/setuptools-83.0.0", "0");
        let atoms = vec![
            "dev-python/setuptools".to_string(),
            "dev-python/pip".to_string(),
        ];
        assert_eq!(mass_best_version(&vdb.eroot, &atoms).unwrap(), 0);
    }

    #[test]
    fn cpv_ordering_picks_the_highest_version() {
        // Directly exercises the same `.max()` call best_version/
        // mass_best_version rely on, with an assertable return value
        // (println! output isn't easily captured in-process).
        let low = Cpv::parse("dev-python/setuptools-79.0.1").unwrap();
        let high = Cpv::parse("dev-python/setuptools-83.0.0").unwrap();
        assert_eq!(vec![&low, &high].into_iter().max(), Some(&high));
    }

    #[test]
    fn slot_file_is_written_and_readable() {
        // Sanity check on the TestVdb fixture itself: a real InstalledPackage
        // built from it must resolve the SLOT file `matches()` depends on.
        let vdb = TestVdb::new();
        vdb.add("dev-python/setuptools-83.0.0", "1");
        let pkgs = installed_packages(&vdb.eroot);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].slot().unwrap().to_string(), "1");
    }

    #[test]
    fn missing_operands_report_exit_2() {
        let mut out = Vec::new();
        assert_eq!(
            crate::portageq::check_arity(&[], &mut out).unwrap(),
            Some(2)
        );
        assert_eq!(out, b"ERROR: insufficient parameters!\n");
        let mut out = Vec::new();
        assert_eq!(
            crate::portageq::check_arity(&["CFLAGS".to_string()], &mut out).unwrap(),
            None
        );
        assert!(out.is_empty());
    }

    #[test]
    fn envvar_verbose_quotes_only_when_needed() {
        assert_eq!(
            crate::portageq::format_verbose("PORTAGE_TMPDIR", "/var/tmp"),
            "PORTAGE_TMPDIR=/var/tmp"
        );
        assert_eq!(
            crate::portageq::format_verbose("CFLAGS", "-O2 -march=native -pipe"),
            "CFLAGS='-O2 -march=native -pipe'"
        );
        assert_eq!(
            crate::portageq::format_verbose("FOO", "a'b"),
            "FOO='a'\"'\"'b'"
        );
    }

    #[test]
    fn envvar_write_values_mixed_hit_and_miss() {
        let names = vec!["CFLAGS".to_string(), "NOPE_UNSET_XYZ".to_string()];
        let values = vec![Some("-O2 -pipe".to_string()), None];
        let mut out = Vec::new();
        assert_eq!(
            crate::portageq::write_values(&names, false, &values, &mut out).unwrap(),
            1
        );
        assert_eq!(out, b"-O2 -pipe\n\n");
        let mut out_v = Vec::new();
        assert_eq!(
            crate::portageq::write_values(&names, true, &values, &mut out_v).unwrap(),
            1
        );
        assert_eq!(out_v, b"CFLAGS='-O2 -pipe'\n\n");
    }

    /// Hermetic load: a missing global path is skipped, the user conf
    /// alone decides membership and order.
    fn test_conf() -> (tempfile::TempDir, portage_repo::ReposConf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        std::fs::create_dir_all(format!("{root}/user")).unwrap();
        std::fs::write(
            format!("{root}/user/test.conf"),
            format!("[mmm]\nlocation = {root}/mmm\n\n[zzz]\nlocation = {root}/zzz\n"),
        )
        .unwrap();
        let conf = crate::portageq::repos_conf_from(
            &Utf8PathBuf::from(format!("{root}/noglobal.conf")),
            Some(&Utf8PathBuf::from(format!("{root}/user"))),
            &[],
        )
        .unwrap();
        (tmp, conf)
    }

    #[test]
    fn user_repos_resolve_in_reverse_order() {
        let (_tmp, conf) = test_conf();
        let names: Vec<String> = conf
            .repos()
            .iter()
            .rev()
            .map(|r| r.name.to_string())
            .collect();
        assert_eq!(names, ["zzz", "mmm"]);
    }

    #[test]
    fn overlay_fallback_lists_non_main_repos() {
        let (tmp, conf) = test_conf();
        let root = tmp.path().to_string_lossy().into_owned();
        assert_eq!(
            crate::portageq::fallback_overlays(&conf),
            [format!("{root}/mmm"), format!("{root}/zzz")]
        );
    }

    #[test]
    fn repo_name_validation_matches_portage() {
        for valid in ["gentoo", "guru", "bad-name", "bad_name", "BadName"] {
            assert!(crate::portageq::is_valid_repo_name(valid), "{valid}");
        }
        for invalid in ["bad/name", "bad.name", "a b", ""] {
            assert!(!crate::portageq::is_valid_repo_name(invalid), "{invalid}");
        }
    }

    /// Temp eroot with its own `repos.conf`: two repos, no main-repo.
    struct TestRepos {
        _tmp: tempfile::TempDir,
        eroot: String,
    }

    impl TestRepos {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let eroot = tmp.path().to_string_lossy().into_owned();
            let conf = format!("[mmm]\nlocation = {eroot}/mmm\n\n[zzz]\nlocation = {eroot}/zzz\n");
            let dir = std::path::Path::new(&eroot).join("etc/portage/repos.conf");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("test.conf"), conf).unwrap();
            Self { _tmp: tmp, eroot }
        }
    }

    #[test]
    fn get_repos_missing_eroot_is_64() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            crate::portageq::get_repos("/no/such/eroot", &mut out, &mut err).unwrap(),
            64
        );
        assert!(out.is_empty());
    }

    #[test]
    fn get_repo_path_without_repos_is_3() {
        let r = TestRepos::new();
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            crate::portageq::get_repo_path(&r.eroot, &[], &mut out, &mut err).unwrap(),
            3
        );
        assert!(out.is_empty());
        assert_eq!(err, b"ERROR: insufficient parameters!\n");
    }

    #[test]
    fn get_repo_path_hit_miss_and_invalid() {
        let r = TestRepos::new();
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            crate::portageq::get_repo_path(&r.eroot, &["mmm".to_string()], &mut out, &mut err)
                .unwrap(),
            0
        );
        assert_eq!(out, format!("{}/mmm\n", r.eroot).as_bytes());

        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            crate::portageq::get_repo_path(&r.eroot, &["nope".to_string()], &mut out, &mut err)
                .unwrap(),
            1
        );
        assert_eq!(out, b"\n");

        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            crate::portageq::get_repo_path(
                &r.eroot,
                &["mmm".to_string(), "bad/name".to_string()],
                &mut out,
                &mut err
            )
            .unwrap(),
            2
        );
        assert_eq!(out, format!("{}/mmm\n", r.eroot).as_bytes());
    }
}
