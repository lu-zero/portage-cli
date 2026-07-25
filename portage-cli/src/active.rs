//! Persistent "active" `--prefix` / `--local` for dogfooding.
//!
//! Phase 3 of `todo/select-toolchain.md`: register a default topology so bare
//! `em <pkg>` picks up a prefix/local without repeating flags every time.
//! Explicit `--prefix` / `--local` / `--root` always win over the registration.
//!
//! State lives under `$XDG_STATE_HOME/em/active` (default
//! `~/.local/state/em/active`). `em active env` prints shell exports so an
//! interactive session can also put the prefix's `usr/bin` on `PATH`.

use std::fmt;
use std::io::Write;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

use crate::cli::{ActiveCommand, Cli};
use crate::util::write_atomic;

/// Kind of registered active topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveKind {
    /// `--prefix` overlay (BROOT stays the host; install target is the path).
    Prefix,
    /// `--local` standalone Gentoo-Prefix (own BROOT + EPREFIX).
    Local,
}

impl ActiveKind {
    fn as_str(self) -> &'static str {
        match self {
            ActiveKind::Prefix => "prefix",
            ActiveKind::Local => "local",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "prefix" => Some(ActiveKind::Prefix),
            "local" => Some(ActiveKind::Local),
            _ => None,
        }
    }
}

impl fmt::Display for ActiveKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Registered active topology (absolute path + kind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveContext {
    pub kind: ActiveKind,
    pub path: Utf8PathBuf,
}

impl ActiveContext {
    /// Human-readable one-liner for `em active show`.
    pub fn display_line(&self) -> String {
        format!("{} {}", self.kind, self.path)
    }
}

/// `$XDG_STATE_HOME/em`, or `~/.local/state/em` when unset.
///
/// Override via `XDG_STATE_HOME` (tests pin this to a temp dir).
pub fn state_dir() -> Utf8PathBuf {
    crate::xdg::em_state_dir()
}

/// Path of the active-state file.
pub fn state_file() -> Utf8PathBuf {
    state_dir().join("active")
}

/// Load the registered active context, if any.
///
/// Returns `Ok(None)` when no file exists or the file is empty. Malformed
/// content is an error so a corrupted registration is not silently ignored.
pub fn load() -> Result<Option<ActiveContext>> {
    let path = state_file();
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path.as_std_path())
        .with_context(|| format!("reading active state {path}"))?;
    parse_state(&text)
}

/// Persist `ctx` as the active registration.
pub fn save(ctx: &ActiveContext) -> Result<()> {
    let dir = state_dir();
    std::fs::create_dir_all(dir.as_std_path())
        .with_context(|| format!("creating active state dir {dir}"))?;
    let body = format!(
        "# em active state — written by `em active set`\nkind={}\npath={}\n",
        ctx.kind.as_str(),
        ctx.path
    );
    write_atomic(&state_file(), body)
}

/// Remove the active registration. No-op if none is set.
pub fn clear() -> Result<bool> {
    let path = state_file();
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(path.as_std_path())
        .with_context(|| format!("removing active state {path}"))?;
    Ok(true)
}

fn parse_state(text: &str) -> Result<Option<ActiveContext>> {
    let mut kind: Option<ActiveKind> = None;
    let mut path: Option<Utf8PathBuf> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            bail!("active state: expected key=value, got {raw:?}");
        };
        match k.trim() {
            "kind" => {
                let v = v.trim();
                kind = Some(
                    ActiveKind::parse(v)
                        .ok_or_else(|| anyhow::anyhow!("active state: unknown kind {v:?}"))?,
                );
            }
            "path" => {
                let v = v.trim();
                if v.is_empty() {
                    bail!("active state: empty path");
                }
                path = Some(Utf8PathBuf::from(v));
            }
            other => bail!("active state: unknown key {other:?}"),
        }
    }
    match (kind, path) {
        (None, None) => Ok(None),
        (Some(kind), Some(path)) => {
            if !path.is_absolute() {
                bail!("active state: path must be absolute, got {path}");
            }
            Ok(Some(ActiveContext { kind, path }))
        }
        _ => bail!("active state: both kind= and path= are required"),
    }
}

/// Resolve a user-supplied path to an absolute UTF-8 path.
///
/// Canonicalizes when the path exists so `../foo` registrations stay stable;
/// otherwise joins against the current working directory.
pub fn absolutize(path: &Utf8Path) -> Result<Utf8PathBuf> {
    if path.as_str().is_empty() {
        bail!("active path must not be empty");
    }
    if path.exists() {
        let canon = path
            .canonicalize_utf8()
            .with_context(|| format!("canonicalizing {path}"))?;
        return Ok(canon);
    }
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    let cwd = std::env::current_dir().context("reading current directory")?;
    let joined = cwd.join(path.as_str());
    Utf8PathBuf::from_path_buf(joined)
        .map_err(|p| anyhow::anyhow!("active path is not valid UTF-8: {}", p.display()))
}

/// Default `--local` path (`~/.gentoo`), matching [`Cli`]'s flag semantics.
pub fn default_local_path() -> Utf8PathBuf {
    crate::xdg::home().join(".gentoo")
}

/// Shell snippet suitable for `eval "$(em active env)"` (bash/zsh).
///
/// Prepends the prefix's `usr/bin` and `bin` to `PATH` and exports
/// `EM_ACTIVE_KIND` / `EM_ACTIVE_PATH` markers. Does **not** set `EPREFIX`
/// globally — that would confuse host tools; `em` itself reads the state file.
pub fn env_exports(ctx: &ActiveContext) -> String {
    let p = ctx.path.as_str();
    // Escape path components for use inside double quotes, then append the
    // `${PATH:+:$PATH}` expansion *unescaped* so the shell still expands it.
    let bin1 = shell_escape_double_inner(&format!("{p}/usr/bin"));
    let bin2 = shell_escape_double_inner(&format!("{p}/bin"));
    format!(
        "# em active env — eval this in bash/zsh\n\
         export EM_ACTIVE_KIND={}\n\
         export EM_ACTIVE_PATH={}\n\
         export PATH=\"{bin1}:{bin2}${{PATH:+:$PATH}}\"\n",
        ctx.kind.as_str(),
        shell_double_quote(p),
    )
}

/// Double-quote a string for POSIX shell (escape `\`, `"`, `$`, `` ` ``).
fn shell_double_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&shell_escape_double_inner(s));
    out.push('"');
    out
}

/// Escape characters that are special inside double-quoted POSIX shell strings.
fn shell_escape_double_inner(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '"' | '$' | '`' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Dispatch `em active …`.
pub fn run(command: Option<&ActiveCommand>, globals: &Cli) -> Result<()> {
    match command {
        None | Some(ActiveCommand::Show) => run_show(),
        Some(ActiveCommand::Set) => run_set(globals),
        Some(ActiveCommand::Clear) => run_clear(),
        Some(ActiveCommand::Env) => run_env(),
    }
}

fn run_show() -> Result<()> {
    match load()? {
        Some(ctx) => {
            println!("{}", ctx.display_line());
            Ok(())
        }
        None => {
            println!("(no active prefix/local registered)");
            println!("Register one with: em --prefix DIR active set");
            println!("                 or: em --local= active set");
            println!("                 or: em --local /path active set");
            Ok(())
        }
    }
}

fn run_clear() -> Result<()> {
    if clear()? {
        println!("cleared active context");
    } else {
        println!("(no active context to clear)");
    }
    Ok(())
}

fn run_env() -> Result<()> {
    let Some(ctx) = load()? else {
        bail!(
            "no active prefix/local registered — run \
             `em --prefix DIR active set` (or `em --local active set`) first"
        );
    };
    let mut out = std::io::stdout().lock();
    write!(out, "{}", env_exports(&ctx))?;
    Ok(())
}

fn run_set(globals: &Cli) -> Result<()> {
    let ctx = resolve_set_target(globals)?;
    // Warn (don't fail) when the path does not exist yet — user may set
    // before `em setup` / first build.
    if !ctx.path.exists() {
        eprintln!(
            "warning: {} does not exist yet — register it anyway (run em setup first if needed)",
            ctx.path
        );
    }
    save(&ctx)?;
    println!("active {} → {}", ctx.kind, ctx.path);
    println!("bare `em` now uses this context; override with --prefix/--local/--root");
    println!("shell PATH: eval \"$(em active env)\"");
    Ok(())
}

/// Resolve what `em active set` should register from the global flags.
///
/// Precedence: `--local` > `--prefix` (matches [`Cli::topology_source`]).
/// `--root` is intentionally not registerable — active is for unprivileged
/// prefix/local dogfooding only.
fn resolve_set_target(globals: &Cli) -> Result<ActiveContext> {
    if let Some(local) = globals.local.as_deref() {
        let path = if local.is_empty() {
            default_local_path()
        } else {
            absolutize(Utf8Path::new(local))?
        };
        let path = finalize_abs_path(path)?;
        return Ok(ActiveContext {
            kind: ActiveKind::Local,
            path,
        });
    }
    if let Some(p) = globals.prefix.as_deref() {
        let path = finalize_abs_path(absolutize(Utf8Path::new(p))?)?;
        return Ok(ActiveContext {
            kind: ActiveKind::Prefix,
            path,
        });
    }
    bail!(
        "em active set needs a target: pass --prefix DIR or --local [DIR]\n\
         examples:\n  em --prefix /home/me/prefix active set\n  em --local= active set\n  em --local /other active set\n\
         note: bare `em --local active set` steals `active` as the path — use `em --local=` or put a path"
    );
}

/// Canonicalize when the path exists; otherwise keep the absolute form.
fn finalize_abs_path(path: Utf8PathBuf) -> Result<Utf8PathBuf> {
    if path.exists() {
        path.canonicalize_utf8()
            .with_context(|| format!("canonicalizing {path}"))
    } else if path.is_absolute() {
        Ok(path)
    } else {
        absolutize(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::home_lock;
    use clap::Parser;

    /// Pin `XDG_STATE_HOME` (and optionally `HOME`) for the duration of a test.
    struct StateGuard {
        _home: std::sync::MutexGuard<'static, ()>,
        saved_xdg: Option<String>,
        saved_home: Option<String>,
    }

    impl StateGuard {
        fn new(state_parent: &Utf8Path, home: Option<&Utf8Path>) -> Self {
            let _home = home_lock();
            let saved_xdg = std::env::var("XDG_STATE_HOME").ok();
            let saved_home = std::env::var("HOME").ok();
            // SAFETY: held under home_lock; no other test mutates these.
            unsafe {
                std::env::set_var("XDG_STATE_HOME", state_parent.as_str());
                if let Some(h) = home {
                    std::env::set_var("HOME", h.as_str());
                }
            }
            Self {
                _home,
                saved_xdg,
                saved_home,
            }
        }
    }

    impl Drop for StateGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.saved_xdg {
                    Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                    None => std::env::remove_var("XDG_STATE_HOME"),
                }
                match &self.saved_home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    #[test]
    fn parse_round_trip() {
        let text = "# comment\nkind=prefix\npath=/opt/p\n";
        let ctx = parse_state(text).unwrap().unwrap();
        assert_eq!(ctx.kind, ActiveKind::Prefix);
        assert_eq!(ctx.path.as_str(), "/opt/p");
    }

    #[test]
    fn parse_local() {
        let ctx = parse_state("kind=local\npath=/home/u/.gentoo\n")
            .unwrap()
            .unwrap();
        assert_eq!(ctx.kind, ActiveKind::Local);
    }

    #[test]
    fn parse_empty_is_none() {
        assert!(parse_state("# only comments\n\n").unwrap().is_none());
    }

    #[test]
    fn parse_rejects_relative_path() {
        assert!(parse_state("kind=prefix\npath=rel/path\n").is_err());
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        assert!(parse_state("kind=root\npath=/tmp/x\n").is_err());
    }

    #[test]
    fn save_load_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let _g = StateGuard::new(&parent, None);

        assert!(load().unwrap().is_none());

        let ctx = ActiveContext {
            kind: ActiveKind::Prefix,
            path: Utf8PathBuf::from("/tmp/my-prefix"),
        };
        save(&ctx).unwrap();
        assert_eq!(load().unwrap().as_ref(), Some(&ctx));

        assert!(clear().unwrap());
        assert!(load().unwrap().is_none());
        assert!(!clear().unwrap());
    }

    #[test]
    fn env_exports_contain_path_and_kind() {
        let ctx = ActiveContext {
            kind: ActiveKind::Local,
            path: Utf8PathBuf::from("/home/me/.gentoo"),
        };
        let s = env_exports(&ctx);
        assert!(s.contains("EM_ACTIVE_KIND=local"));
        assert!(s.contains("/home/me/.gentoo"));
        assert!(s.contains("/home/me/.gentoo/usr/bin"));
        assert!(s.contains("export PATH="));
    }

    #[test]
    fn set_via_global_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let prefix = parent.join("gpfx");
        std::fs::create_dir_all(prefix.as_std_path()).unwrap();
        let _g = StateGuard::new(&parent, None);

        let cli = Cli::parse_from(["em", "--prefix", prefix.as_str(), "active", "set"]);
        match &cli.applet {
            Some(crate::cli::Applet::Active { command }) => {
                run(command.as_ref(), &cli).unwrap();
            }
            _ => panic!("expected Active applet"),
        }
        let loaded = load().unwrap().unwrap();
        assert_eq!(loaded.kind, ActiveKind::Prefix);
        assert_eq!(loaded.path, prefix.canonicalize_utf8().unwrap());
    }

    #[test]
    fn set_local_default_uses_home_gentoo() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let home = parent.join("home");
        std::fs::create_dir_all(home.as_std_path()).unwrap();
        let _g = StateGuard::new(&parent, Some(&home));

        // `--local` has an optional value (`num_args = 0..=1`); a bare
        // `--local active set` would steal `active` as the DIR. Use the
        // equals-empty form so the default `~/.gentoo` kicks in, matching
        // interactive `em --local -- active set` / `em --local= active set`.
        let cli = Cli::parse_from(["em", "--local=", "active", "set"]);
        assert_eq!(
            cli.local.as_deref(),
            Some(""),
            "default --local must be empty string"
        );
        match &cli.applet {
            Some(crate::cli::Applet::Active { command }) => {
                run(command.as_ref(), &cli).unwrap();
            }
            _ => panic!("expected Active applet"),
        }
        let loaded = load().unwrap().unwrap();
        assert_eq!(loaded.kind, ActiveKind::Local);
        assert_eq!(loaded.path, home.join(".gentoo"));
    }

    #[test]
    fn set_without_target_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let _g = StateGuard::new(&parent, None);
        let cli = Cli::parse_from(["em", "active", "set"]);
        match &cli.applet {
            Some(crate::cli::Applet::Active { command }) => {
                let err = run(command.as_ref(), &cli).unwrap_err();
                assert!(
                    err.to_string().contains("needs a target"),
                    "unexpected error: {err}"
                );
            }
            _ => panic!("expected Active applet"),
        }
    }

    #[test]
    fn shell_double_quote_escapes() {
        assert_eq!(shell_double_quote(r#"a$b"c`d\e"#), r#""a\$b\"c\`d\\e""#);
    }

    #[test]
    fn env_exports_leave_path_expansion_unescaped() {
        let ctx = ActiveContext {
            kind: ActiveKind::Prefix,
            path: Utf8PathBuf::from("/opt/p"),
        };
        let s = env_exports(&ctx);
        // The shell must still see ${PATH:+:$PATH}, not \${PATH…}.
        assert!(
            s.contains(r#"export PATH="/opt/p/usr/bin:/opt/p/bin${PATH:+:$PATH}""#),
            "unexpected env export:\n{s}"
        );
    }
}
