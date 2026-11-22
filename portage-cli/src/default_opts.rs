//! `EMERGE_DEFAULT_OPTS` and `EM_DEFAULT_OPTS` for an emerge run.
//!
//! The argv is the kept words of `EMERGE_DEFAULT_OPTS`, then `EM_DEFAULT_OPTS`,
//! then the command line. A scalar flag's last value wins. Unknown flags are
//! dropped from the first variable and rejected from the second.

use std::ffi::OsString;
use std::sync::OnceLock;

use camino::Utf8Path;
use winnow_args::{Args, Error, ErrorKind};

use crate::cli::{Applet, Cli, Validated};

const EMERGE: &str = "EMERGE_DEFAULT_OPTS";
const EM: &str = "EM_DEFAULT_OPTS";

static IGNORED: OnceLock<Vec<String>> = OnceLock::new();

/// One configured variable after splitting and dropping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportedVar {
    pub name: &'static str,
    pub source: String,
    pub kept: Vec<String>,
    pub dropped: Vec<String>,
}

/// What an emerge run would insert, and where each flag came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Explanation {
    pub variables: Vec<ReportedVar>,
    /// Kept words, `EMERGE_DEFAULT_OPTS` then `EM_DEFAULT_OPTS`.
    pub effective: Vec<String>,
}

impl Explanation {
    /// `EMERGE_DEFAULT_OPTS: -j80 -l80 (ignored: --alert) [environment]`,
    /// then the combined `effective:` line.
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = self
            .variables
            .iter()
            .map(ReportedVar::summary)
            .collect::<Vec<_>>();
        if !self.effective.is_empty() {
            lines.push(format!("effective: {}", self.effective.join(" ")));
        }
        lines
    }

    fn emerge_ignored(&self) -> Vec<String> {
        self.variables
            .iter()
            .filter(|var| var.name == EMERGE)
            .flat_map(|var| var.dropped.iter().cloned())
            .collect()
    }
}

impl ReportedVar {
    fn summary(&self) -> String {
        let mut body = self.kept.join(" ");
        if !self.dropped.is_empty() {
            let ignored = format!("(ignored: {})", self.dropped.join(" "));
            if body.is_empty() {
                body = ignored;
            } else {
                body.push(' ');
                body.push_str(&ignored);
            }
        }
        format!("{}: {body} [{}]", self.name, self.source)
    }
}

/// Parse `args` (after the program name). An emerge run without
/// `--ignore-default-opts` is parsed again with the default options inserted.
pub(crate) fn parse(args: &[OsString]) -> Result<Validated, Error> {
    let (validated, explanation) = apply(args, &process_env)?;
    let _ = IGNORED.set(explanation.emerge_ignored());
    Ok(validated)
}

/// Dropped `EMERGE_DEFAULT_OPTS` words. No line when nothing was dropped.
/// The subscriber has to be installed first: this runs from `main` after it.
pub fn log_ignored() {
    let Some(words) = IGNORED.get() else {
        return;
    };
    if words.is_empty() {
        return;
    }
    tracing::debug!("EMERGE_DEFAULT_OPTS ignored: {}", words.join(" "));
}

/// The configured defaults for `cli`, including when this run ignores them.
pub(crate) fn explain(cli: &Cli) -> Result<Explanation, Error> {
    load(cli, &process_env)
}

fn apply(
    args: &[OsString],
    env: &dyn Fn(&str) -> Result<Option<String>, Error>,
) -> Result<(Validated, Explanation), Error> {
    let cli = Cli::parse_from(args)?;
    if !uses_defaults(&cli) {
        return Ok((Validated::try_from(cli)?, Explanation::default()));
    }
    let explanation = load(&cli, env)?;
    if explanation.effective.is_empty() {
        return Ok((Validated::try_from(cli)?, explanation));
    }
    let argv = insert(args, &explanation.effective);
    let cli = Cli::parse_from(&argv)?;
    Ok((Validated::try_from(cli)?, explanation))
}

fn uses_defaults(cli: &Cli) -> bool {
    match &cli.applet {
        Some(Applet::Emerge(args)) => !args.ignore_default_opts,
        _ => false,
    }
}

fn load(
    cli: &Cli,
    env: &dyn Fn(&str) -> Result<Option<String>, Error>,
) -> Result<Explanation, Error> {
    let roots = cli.roots();
    let config = roots.config().unwrap_or(Utf8Path::new("/"));
    let mut variables = Vec::new();
    let mut effective = Vec::new();
    for (name, strict) in [(EMERGE, false), (EM, true)] {
        let Some(spec) = source_of(name, config, roots.config_overlay(), env)? else {
            continue;
        };
        let class = classify(name, &spec.value, strict)?;
        if class.kept.is_empty() && class.dropped.is_empty() {
            continue;
        }
        effective.extend(class.kept.iter().cloned());
        variables.push(ReportedVar {
            name,
            source: spec.source,
            kept: class.kept,
            dropped: class.dropped,
        });
    }
    Ok(Explanation {
        variables,
        effective,
    })
}

struct Spec {
    value: String,
    source: String,
}

fn source_of(
    name: &str,
    config: &Utf8Path,
    overlay: Option<&Utf8Path>,
    env: &dyn Fn(&str) -> Result<Option<String>, Error>,
) -> Result<Option<Spec>, Error> {
    if let Some(value) = env(name)? {
        return Ok(Some(Spec {
            value,
            source: "environment".to_owned(),
        }));
    }
    let mut vars = std::collections::BTreeMap::new();
    let mut found = None;
    for path in candidates(config, overlay) {
        let Some(conf) = make_conf(&path, name)? else {
            continue;
        };
        conf.expand_assignments(&mut vars);
        if conf.get(name).is_some() {
            found = vars.get(name).map(|value| Spec {
                value: value.clone(),
                source: path.to_string(),
            });
        }
    }
    Ok(found)
}

fn candidates(config: &Utf8Path, overlay: Option<&Utf8Path>) -> Vec<camino::Utf8PathBuf> {
    let mut paths = vec![
        config.join("etc/make.conf"),
        config.join("etc/portage/make.conf"),
        config.join("etc/portage/make.conf.d"),
    ];
    if let Some(overlay) = overlay {
        let portage = config.join("etc/portage");
        if overlay != portage.as_path() {
            paths.push(overlay.join("make.conf"));
            paths.push(overlay.join("make.conf.d"));
        }
    }
    paths
}

fn make_conf(path: &Utf8Path, name: &str) -> Result<Option<portage_repo::MakeConf>, Error> {
    match portage_repo::MakeConf::load(path) {
        Ok(conf) => Ok(Some(conf)),
        Err(portage_repo::Error::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(err) => Err(Error::invalid(format!("reading {name} from {path}: {err}"))),
    }
}

fn process_env(name: &str) -> Result<Option<String>, Error> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(Error::invalid(format!("{name}: value is not UTF-8")))
        }
    }
}

#[derive(Debug)]
struct Classified {
    kept: Vec<String>,
    dropped: Vec<String>,
}

fn classify(name: &str, raw: &str, strict: bool) -> Result<Classified, Error> {
    let words = split_words(name, raw)?;
    let words = if strict {
        words
    } else {
        portage_spellings(words)
    };
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if !looks_like_flag(&words[i]) {
            if strict {
                return Err(Error::invalid(format!(
                    "{name}: unexpected word `{}`",
                    words[i]
                )));
            }
            dropped.push(words[i].clone());
            i += 1;
            continue;
        }
        let (group, next) = group_at(name, &words, i)?;
        let group = match boolean_value(&group).filter(|_| !strict) {
            Some((flag, true)) => vec![flag.to_owned()],
            Some((flag, false)) => {
                kept.retain(|word| word != flag);
                dropped.extend(group);
                i = next;
                continue;
            }
            None => group,
        };
        match probe(&group) {
            // Decided on the first parse. Inserting a copy would mark the run
            // as ignored after the options had already been applied.
            Ok(()) if group.iter().any(|word| word == "--ignore-default-opts") => {
                if strict {
                    return Err(Error::invalid(format!(
                        "{name}: --ignore-default-opts is only read from the command line"
                    )));
                }
                dropped.extend(group);
            }
            Ok(()) => kept.extend(group),
            // A bare `--jobs` means unlimited in Portage, which `em` cannot express.
            Err(err)
                if !strict
                    && matches!(err.kind(), ErrorKind::UnknownFlag | ErrorKind::MissingValue) =>
            {
                dropped.extend(group)
            }
            Err(err) => return Err(refuse(name, &group, err)),
        }
        i = next;
    }
    Ok(Classified { kept, dropped })
}

/// Rewrites Portage's `--flag y`, `--flag=True` and `--color=y|n` to `--flag=y|n` or `em`'s value.
fn portage_spellings(words: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    let mut words = words.into_iter().peekable();
    while let Some(word) = words.next() {
        let bare_long = word.len() > 2 && word.starts_with("--") && !word.contains('=');
        let value = bare_long
            .then(|| words.next_if(|next| matches!(next.as_str(), "y" | "n" | "True")))
            .flatten();
        let word = value.map_or(word.clone(), |value| format!("{word}={value}"));
        out.push(match word.as_str() {
            "--color=y" | "--color=True" => "--color=always".to_owned(),
            "--color=n" => "--color=never".to_owned(),
            _ => match word.strip_suffix("=True") {
                Some(flag) => format!("{flag}=y"),
                None => word,
            },
        });
    }
    out
}

/// `(--flag, on)` for `--flag=y|n` on a flag `em` parses as a plain switch.
fn boolean_value(group: &[String]) -> Option<(&str, bool)> {
    let [word] = group else {
        return None;
    };
    let (flag, value) = word.split_once('=')?;
    let on = match value {
        "y" => true,
        "n" => false,
        _ => return None,
    };
    let err = probe(group).err()?;
    (err.kind() == ErrorKind::UnexpectedValue).then_some((flag, on))
}

fn group_at(name: &str, words: &[String], i: usize) -> Result<(Vec<String>, usize), Error> {
    let word = words[i].clone();
    if word.contains('=') || words.get(i + 1).is_none_or(|next| looks_like_flag(next)) {
        return Ok((vec![word], i + 1));
    }
    match probe(std::slice::from_ref(&word)) {
        Ok(()) => Ok((vec![word], i + 1)),
        Err(err)
            if err.kind() == ErrorKind::UnknownFlag || err.kind() == ErrorKind::MissingValue =>
        {
            Ok((vec![word, words[i + 1].clone()], i + 2))
        }
        Err(err) => Err(refuse(name, std::slice::from_ref(&word), err)),
    }
}

fn looks_like_flag(word: &str) -> bool {
    match word.strip_prefix('-') {
        Some(rest) => !rest.is_empty() && rest != "-",
        None => false,
    }
}

fn probe(group: &[String]) -> Result<(), Error> {
    let mut args = Vec::with_capacity(group.len() + 1);
    args.push("emerge".to_owned());
    args.extend(group.iter().cloned());
    Cli::parse_from(args).map(|_| ())
}

fn split_words(name: &str, raw: &str) -> Result<Vec<String>, Error> {
    winnow_args::response::split(raw)
        .map(|words| words.into_iter().map(|word| word.into_owned()).collect())
        .map_err(|err| Error::invalid(format!("{name}: {err}")))
}

fn refuse(name: &str, group: &[String], err: Error) -> Error {
    let rendered = err.render(winnow_args::help::Style::PLAIN);
    Error::invalid(format!("{name}: {}: {rendered}", group.join(" ")))
}

fn insert(args: &[OsString], words: &[String]) -> Vec<OsString> {
    let at = applet_word(args, words).map_or(0, |word| word + 1);
    splice(args, at, words)
}

/// The `emerge` word naming the applet, not a flag's value or an atom.
fn applet_word(args: &[OsString], words: &[String]) -> Option<usize> {
    let atoms = Cli::parse_from(args).ok()?.atoms();
    let same_atoms = |argv: &[OsString]| Cli::parse_from(argv).is_ok_and(|c| c.atoms() == atoms);
    args.iter()
        .take_while(|arg| *arg != "--")
        .enumerate()
        .filter(|(_, arg)| *arg == "emerge")
        .map(|(i, _)| i)
        .find(|&i| {
            let mut without = args.to_vec();
            without.remove(i);
            same_atoms(&without) && same_atoms(&splice(args, i + 1, words))
        })
}

fn splice(args: &[OsString], at: usize, words: &[String]) -> Vec<OsString> {
    let mut out = Vec::with_capacity(args.len() + words.len());
    out.extend(args[..at].iter().cloned());
    out.extend(words.iter().map(OsString::from));
    out.extend(args[at..].iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(
        emerge: Option<&str>,
        em: Option<&str>,
    ) -> impl Fn(&str) -> Result<Option<String>, Error> {
        let emerge = emerge.map(str::to_owned);
        let em = em.map(str::to_owned);
        move |name| {
            Ok(match name {
                EMERGE => emerge.clone(),
                EM => em.clone(),
                _ => None,
            })
        }
    }

    fn no_env(name: &str) -> Result<Option<String>, Error> {
        let _ = name;
        Ok(None)
    }

    fn scratch(rest: &[&str]) -> (tempfile::TempDir, Vec<OsString>) {
        let dir = tempfile::tempdir().unwrap();
        // `--root` keeps an active prefix from supplying its own make.conf.
        let mut args = vec![
            OsString::from("--root"),
            OsString::from(dir.path()),
            OsString::from("--config-root"),
            OsString::from(dir.path()),
        ];
        args.extend(rest.iter().map(OsString::from));
        (dir, args)
    }

    fn write_conf(root: &std::path::Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn show(args: &[OsString]) -> String {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn splitter_keeps_quotes_and_names_a_broken_value() {
        let class = classify(EMERGE, r#"-j4 '--load-average=8'"#, false).unwrap();
        assert_eq!(class.kept, ["-j4", "--load-average=8"]);

        let err = classify(EMERGE, "'--jobs=4", false).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(EMERGE) && text.contains("unclosed"), "{text}");

        let err = classify(EM, "--jobs=4\\", true).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(EM) && text.contains("trailing"), "{text}");
    }

    #[test]
    fn unknown_groups_are_dropped_and_a_bad_value_is_an_error() {
        let class = classify(EMERGE, "--alert=yes --alert foo --jobs=4 --alert", false).unwrap();
        assert_eq!(class.kept, ["--jobs=4"]);
        assert_eq!(class.dropped, ["--alert=yes", "--alert", "foo", "--alert"]);

        let class = classify(EMERGE, "--jobs 4 --keep-going", false).unwrap();
        assert_eq!(class.kept, ["--jobs", "4", "--keep-going"]);

        let class = classify(EMERGE, "-j80 -l80 --jobs=65 --load-average 128", false).unwrap();
        assert_eq!(
            class.kept,
            ["-j80", "-l80", "--jobs=65", "--load-average", "128"]
        );

        let err = classify(EMERGE, "--jobs=abc", false).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(EMERGE) && text.contains("jobs"), "{text}");

        let err = classify(EM, "--alert --jobs=4", true).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(EM) && text.contains("alert"), "{text}");
    }

    #[test]
    fn portage_boolean_spellings_are_accepted_from_emerge_only() {
        let class = classify(
            EMERGE,
            "--ask=y --usepkg y --getbinpkg=True --color=n",
            false,
        );
        assert_eq!(
            class.unwrap().kept,
            ["--ask", "--usepkg", "--getbinpkg", "--color=never"]
        );

        let class = classify(EMERGE, "--ask --usepkg --ask=n --usepkg n -v", false).unwrap();
        assert_eq!(class.kept, ["-v"]);
        assert_eq!(class.dropped, ["--ask=n", "--usepkg=n"]);

        let class = classify(EMERGE, "--with-bdeps=True --jobs", false).unwrap();
        assert_eq!(class.kept, ["--with-bdeps=y"]);
        assert_eq!(class.dropped, ["--jobs"]);

        assert!(classify(EM, "--ask=y", true).is_err());
    }

    #[test]
    fn a_stray_word_is_dropped_from_emerge_and_rejected_from_em() {
        let class = classify(EMERGE, "--jobs=4 sys-libs/zlib @jobs", false).unwrap();
        assert_eq!(class.kept, ["--jobs=4"]);
        assert_eq!(class.dropped, ["sys-libs/zlib", "@jobs"]);

        let err = classify(EM, "--jobs=4 zlib", true).unwrap_err();
        assert!(err.to_string().contains("unexpected word `zlib`"));
    }

    #[test]
    fn insert_position_follows_the_first_word() {
        let jobs = ["--jobs=4".to_owned()];
        assert_eq!(
            show(&insert(&os(&["emerge", "-p", "zlib"]), &jobs)),
            "emerge --jobs=4 -p zlib"
        );
        assert_eq!(
            show(&insert(&os(&["-p", "zlib"]), &jobs)),
            "--jobs=4 -p zlib"
        );
        assert_eq!(show(&insert(&os(&["zlib"]), &jobs)), "--jobs=4 zlib");
        assert_eq!(show(&insert(&os(&["-r"]), &jobs)), "--jobs=4 -r");
        assert_eq!(
            show(&insert(
                &os(&["--root", "/x", "emerge", "-p", "zlib"]),
                &jobs
            )),
            "--root /x emerge --jobs=4 -p zlib"
        );
        assert_eq!(
            show(&insert(&os(&["--root", "emerge", "emerge", "zlib"]), &jobs)),
            "--root emerge emerge --jobs=4 zlib"
        );
        assert_eq!(
            show(&insert(&os(&["zlib", "emerge"]), &jobs)),
            "--jobs=4 zlib emerge"
        );
    }

    #[test]
    fn command_line_wins_and_em_wins_over_emerge() {
        let (_dir, args) = scratch(&["-p", "--jobs=1", "sys-libs/zlib"]);
        let (validated, _) = apply(&args, &env_of(Some("--jobs=8"), Some("--jobs=4"))).unwrap();
        assert_eq!(validated.0.merge_flags().jobs, Some(1));
        assert!(validated.0.pretend());
        assert_eq!(validated.0.atoms(), ["sys-libs/zlib"]);

        let (_dir, args) = scratch(&["-p", "sys-libs/zlib"]);
        let (validated, explanation) =
            apply(&args, &env_of(Some("--jobs=2"), Some("--jobs=4"))).unwrap();
        assert_eq!(validated.0.merge_flags().jobs, Some(4));
        assert_eq!(
            explanation.lines(),
            [
                "EMERGE_DEFAULT_OPTS: --jobs=2 [environment]",
                "EM_DEFAULT_OPTS: --jobs=4 [environment]",
                "effective: --jobs=2 --jobs=4",
            ]
        );
    }

    #[test]
    fn ignore_default_opts_inside_a_variable_does_not_stick() {
        let class = classify(EMERGE, "--ignore-default-opts --jobs=4", false).unwrap();
        assert_eq!(class.kept, ["--jobs=4"]);
        assert_eq!(class.dropped, ["--ignore-default-opts"]);

        let err = classify(EM, "--ignore-default-opts", true).unwrap_err();
        assert!(
            err.to_string().contains(EM) && err.to_string().contains("command line"),
            "{err}"
        );

        let (_dir, args) = scratch(&["-p", "sys-libs/zlib"]);
        let (validated, explanation) =
            apply(&args, &env_of(Some("--ignore-default-opts --jobs=4"), None)).unwrap();
        assert_eq!(validated.0.merge_flags().jobs, Some(4));
        assert!(
            matches!(
                &validated.0.applet,
                Some(Applet::Emerge(emerge)) if !emerge.ignore_default_opts
            ),
            "a copy inside the variable does not mark the run ignored"
        );
        assert_eq!(explanation.effective, ["--jobs=4"]);
    }

    #[test]
    fn ignore_default_opts_skips_both_variables() {
        let (_dir, args) = scratch(&["--ignore-default-opts", "-p", "sys-libs/zlib"]);
        let (validated, explanation) =
            apply(&args, &env_of(Some("--jobs=4 --quiet"), None)).unwrap();
        assert_eq!(validated.0.merge_flags().jobs, None);
        assert!(!validated.0.quiet());
        assert!(explanation.variables.is_empty());
    }

    #[test]
    fn color_quiet_and_pretend_come_from_the_second_parse() {
        let (_dir, args) = scratch(&["sys-libs/zlib"]);
        let (validated, _) = apply(&args, &env_of(Some("--color=never --quiet -p"), None)).unwrap();
        assert_eq!(validated.0.color, crate::cli::ColorChoice::Never);
        assert!(validated.0.quiet());
        assert!(validated.0.pretend());
        assert_eq!(validated.0.atoms(), ["sys-libs/zlib"]);
    }

    #[test]
    fn environment_overrides_make_conf_and_make_conf_d_overrides_the_file() {
        let (dir, args) = scratch(&["-p", "sys-libs/zlib"]);
        write_conf(
            dir.path(),
            "etc/portage/make.conf",
            "EMERGE_DEFAULT_OPTS=\"--jobs=2\"\n",
        );
        let (validated, explanation) = apply(&args, &env_of(Some("--jobs=9"), None)).unwrap();
        assert_eq!(validated.0.merge_flags().jobs, Some(9));
        assert_eq!(explanation.variables[0].source, "environment");

        let (dir, args) = scratch(&["-p", "sys-libs/zlib"]);
        write_conf(
            dir.path(),
            "etc/portage/make.conf",
            "EMERGE_DEFAULT_OPTS=\"--jobs=2\"\nEMERGE_DEFAULT_OPTS+=\" --quiet\"\n",
        );
        write_conf(
            dir.path(),
            "etc/portage/make.conf.d/zz.conf",
            "EMERGE_DEFAULT_OPTS=\"--jobs=7\"\n",
        );
        let (validated, explanation) = apply(&args, &no_env).unwrap();
        assert_eq!(validated.0.merge_flags().jobs, Some(7));
        assert!(!validated.0.quiet(), "an append is not sourced");
        assert!(explanation.variables[0].source.ends_with("make.conf.d"));
    }

    #[test]
    fn references_expand_across_make_conf_layers() {
        let (dir, args) = scratch(&["-p", "sys-libs/zlib"]);
        write_conf(
            dir.path(),
            "etc/portage/make.conf",
            "J=3\nEMERGE_DEFAULT_OPTS=\"--jobs=${J}\"\n",
        );
        write_conf(
            dir.path(),
            "etc/portage/make.conf.d/zz.conf",
            "EMERGE_DEFAULT_OPTS=\"${EMERGE_DEFAULT_OPTS} --quiet\"\n",
        );
        let (validated, explanation) = apply(&args, &no_env).unwrap();
        assert_eq!(validated.0.merge_flags().jobs, Some(3));
        assert!(validated.0.quiet());
        assert_eq!(explanation.effective, ["--jobs=3", "--quiet"]);
    }

    #[test]
    fn a_prefix_overlay_overrides_the_config_root() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        write_conf(
            dir.path(),
            "etc/portage/make.conf",
            "EMERGE_DEFAULT_OPTS=\"--jobs=2\"\n",
        );
        write_conf(
            &prefix,
            "etc/portage/make.conf",
            "EMERGE_DEFAULT_OPTS=\"--jobs=6\"\n",
        );
        let args = vec![
            OsString::from("--config-root"),
            OsString::from(dir.path()),
            OsString::from("--prefix"),
            OsString::from(&prefix),
            OsString::from("-p"),
            OsString::from("sys-libs/zlib"),
        ];
        let (validated, explanation) = apply(&args, &no_env).unwrap();
        assert_eq!(validated.0.merge_flags().jobs, Some(6));
        assert!(
            explanation.variables[0]
                .source
                .ends_with("prefix/etc/portage/make.conf"),
            "{}",
            explanation.variables[0].source
        );
    }

    #[test]
    fn ignored_line_names_the_dropped_flag() {
        let explanation = Explanation {
            variables: vec![ReportedVar {
                name: EMERGE,
                source: "environment".to_owned(),
                kept: vec!["-j80".to_owned(), "-l80".to_owned()],
                dropped: vec!["--alert".to_owned()],
            }],
            effective: vec!["-j80".to_owned(), "-l80".to_owned()],
        };
        assert_eq!(
            explanation.lines(),
            [
                "EMERGE_DEFAULT_OPTS: -j80 -l80 (ignored: --alert) [environment]",
                "effective: -j80 -l80",
            ]
        );
    }
}
