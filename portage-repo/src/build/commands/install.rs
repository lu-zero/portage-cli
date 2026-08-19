//! `do*`/`new*` install helpers as Rust builtins (PMS 12.3.x)
//!
//! These replace the hand-parsed bash versions in `INSTALL_HELPERS`: real clap
//! arg parsing (`-r`, `-x`), `${ED}`/dest-tree awareness, and the shared
//! [`DieFlag`](super::die::DieFlag) on failure. They shell out to coreutils
//! `install`/`cp`/`ln` for byte-identical mode/ownership semantics — the win is
//! removing fragile bash, not reimplementing install(1).
//!
//! Both families live here as registered builtins; `new*` reads stdin when its
//! source arg is `-` (PMS 12.3.x). The pure destination-state setters
//! (`into`/`insinto`/…) and the `doinitd`/`doconfd`/`doenvd` `do*` wrappers (which
//! set `insinto` then call `doins`) still live in bash.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use brush_core::builtins;
use clap::Parser;

use super::die::DieFlag;
use super::inst_owner::InstOwnerDefaults;

/// The image staging root files install into: `${ED}` (= `${D}${EPREFIX}`),
/// trailing slash trimmed. Falls back to `${D}` then `/` (should not happen
/// once `init_build_env` has run).
fn ed<SE: brush_core::ShellExtensions>(shell: &brush_core::Shell<SE>) -> PathBuf {
    for var in ["ED", "D"] {
        if let Some(v) = shell.env_str(var).filter(|s| !s.is_empty()) {
            return PathBuf::from(v.trim_end_matches('/'));
        }
    }
    PathBuf::from("")
}

fn var<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    name: &str,
    default: &str,
) -> String {
    shell
        .env_str(name)
        .map(|s| s.into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// `${ED}` joined with `rel` (a destination path), leading slash stripped so the
/// join stays under `${ED}` rather than resetting to an absolute path.
fn under_ed(ed: &Path, rel: &str) -> PathBuf {
    ed.join(rel.trim_start_matches('/'))
}

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Look up `name` in the target passwd/group first; if that **file is missing
/// or empty** (pre-baselayout ROOT), fall back to the host `/etc` database so
/// stage/bootstrap builds can still `fowners root:portage` under a fake-root
/// backend. Once the target has a passwd, absent names are hard errors —
/// matching portage's `__helpers_die` (wrong host ids would silently corrupt
/// the image).
fn db_lookup_target_or_host(
    pwdb_etc: &Path,
    file: &str,
    name: &str,
    fields: &[usize],
) -> Option<Vec<String>> {
    let target = pwdb_etc.join(file);
    if let Some(v) = crate::userdb::lookup(&target, name, fields) {
        return Some(v);
    }
    if crate::userdb::is_populated(&target) {
        // Target has a db and the name is simply not there — do not invent
        // host ids (portage hard-dies in this case).
        return None;
    }
    crate::userdb::lookup(Path::new("/etc").join(file).as_path(), name, fields)
}

/// Resolve an `fowners` owner spec (`user[:group]`) to numeric `uid[:gid]`
/// against the *target* `pwdb_etc` (`<ESYSROOT|EROOT>/etc`), mirroring portage's
/// `__resolve_owner`. Numeric components pass through; a name is looked up in
/// `passwd` (uid + primary gid) or `group` (gid). A trailing `:` uses the user's
/// primary group.
///
/// When the target has no passwd/group yet (empty stage, pre-baselayout), names
/// fall back to the host `/etc` so `fowners root:portage` can still resolve
/// under `--privilege pseudoroot|fakeroost`. Once the target db exists, a
/// missing name is a hard error (same as portage).
fn resolve_owner(owner: &str, pwdb_etc: &Path) -> Result<String, String> {
    let (user, group) = match owner.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (owner, None),
    };

    let mut uid = String::new();
    let mut primary_gid: Option<String> = None;
    if !user.is_empty() {
        if is_numeric(user) {
            uid = user.to_string();
        } else {
            let got = db_lookup_target_or_host(pwdb_etc, "passwd", user, &[2, 3]).and_then(|v| {
                match (v.first().cloned(), v.get(1).cloned()) {
                    (Some(u), Some(g)) if is_numeric(&u) => Some((u, g)),
                    _ => None,
                }
            });
            let (u, g) = got.ok_or_else(|| {
                format!(
                    "fowners: invalid user in {}/passwd: {owner}",
                    pwdb_etc.display()
                )
            })?;
            uid = u;
            primary_gid = Some(g);
        }
    }

    let gid = match group {
        None => String::new(),
        Some("") => {
            // `user:` ⇒ chown to the user's primary group.
            let pg = primary_gid.filter(|s| is_numeric(s)).ok_or_else(|| {
                format!(
                    "fowners: invalid primary group for {user} in {}/passwd: {owner}",
                    pwdb_etc.display()
                )
            })?;
            format!(":{pg}")
        }
        Some(g) if is_numeric(g) => format!(":{g}"),
        Some(g) => {
            let gid = db_lookup_target_or_host(pwdb_etc, "group", g, &[2])
                .and_then(|v| v.into_iter().next())
                .filter(|s| is_numeric(s))
                .ok_or_else(|| {
                    format!(
                        "fowners: invalid group in {}/group: {owner}",
                        pwdb_etc.display()
                    )
                })?;
            format!(":{gid}")
        }
    };

    Ok(format!("{uid}{gid}"))
}

/// PMS `get_libdir`: `LIBDIR_${ABI}` if multilib, else `CONF_LIBDIR`, else `lib`
fn get_libdir<SE: brush_core::ShellExtensions>(shell: &brush_core::Shell<SE>) -> String {
    let abi = var(shell, "ABI", "");
    if !abi.is_empty() {
        let v = var(shell, &format!("LIBDIR_{abi}"), "");
        if !v.is_empty() {
            return v;
        }
    }
    let conf = var(shell, "CONF_LIBDIR", "");
    if !conf.is_empty() {
        return conf;
    }
    "lib".to_string()
}

/// `-o <uid> -g <gid>` install args for `dobin`/`dosbin`/`newbin`/`newsbin`
fn inst_owner_install_opts<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
) -> Vec<String> {
    if let Ok(owner) = context.shared::<InstOwnerDefaults>() {
        owner.install_opts(context.shell)
    } else {
        vec![
            "-o".into(),
            var(
                context.shell,
                "PORTAGE_INST_UID",
                &rustix::process::getuid().as_raw().to_string(),
            ),
            "-g".into(),
            var(
                context.shell,
                "PORTAGE_INST_GID",
                &rustix::process::getgid().as_raw().to_string(),
            ),
        ]
    }
}

/// Raise the shared die flag with `msg` and return exit status 1, matching the
/// bash helpers' `|| die "..."`.
fn raise_die<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
    msg: &str,
) -> brush_core::ExecutionResult {
    if let Ok(flag) = context.shared::<DieFlag>() {
        flag.raise(msg);
    }
    let _ = writeln!(context.params.stderr(context.shell), "die: {msg}");
    brush_core::ExecutionResult::new(1)
}

/// Run blocking filesystem work off the async runtime, mapping its `Result` onto
/// success / `die`. Spawned `install`/`cp`/`ln` inherit em's stdio (silent on
/// success); failures surface through the returned message via `raise_die`.
async fn run_blocking<SE, F>(
    context: &brush_core::ExecutionContext<'_, SE>,
    f: F,
) -> brush_core::ExecutionResult
where
    SE: brush_core::ShellExtensions,
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    // Re-enter the current span on the blocking thread so install-helper
    // diagnostics (fowners, …) stay attributed to this package/phase under
    // -j N — spawn_blocking otherwise runs on a thread with an empty span stack.
    let span = tracing::Span::current();
    match tokio::task::spawn_blocking(move || {
        let _span = span.enter();
        f()
    })
    .await
    .unwrap_or_else(|e| Err(format!("task failed: {e}")))
    {
        Ok(()) => brush_core::ExecutionResult::success(),
        Err(msg) => raise_die(context, &msg),
    }
}

// ---- blocking primitives (run inside `run_blocking`'s closure) ----

/// Create `dir` and any missing parents, setting every *newly created*
/// directory to `mode` regardless of the process umask — coreutils
/// `install -d -m<mode>` semantics. Pre-existing components are left untouched
/// (we chmod only what we create), so passing an absolute image path never
/// re-modes `/usr` and friends.
///
/// Pure rustix (mkdir + chmod per component): distinguishes `Ok` (created,
/// so apply `mode`) from `EEXIST` (predecessor already there, leave its mode
/// alone), which is exactly the create-vs-exist distinction coreutils makes.
fn mkdir_p_mode(dir: &Path, mode: u32) -> Result<(), String> {
    use rustix::fs::{Mode, chmod, mkdir};
    use rustix::io::Errno;
    let mode = Mode::from_bits_truncate(mode as _);
    let mut acc = PathBuf::new();
    for comp in dir.components() {
        acc.push(comp);
        // The root component always exists — `mkdir("/")` on an existing
        // root returns EEXIST on Linux (silently absorbed below) but EISDIR
        // on FreeBSD, which isn't; skip it outright rather than relying on
        // errno parity across platforms.
        if acc.as_os_str().is_empty() || matches!(comp, std::path::Component::RootDir) {
            continue;
        }
        match mkdir(&acc, Mode::from_bits_truncate(0o777)) {
            Ok(()) => {
                chmod(&acc, mode).map_err(|e| format!("chmod {}: {e}", acc.display()))?;
            }
            Err(Errno::EXIST) => {}
            Err(e) => return Err(format!("creating {}: {e}", acc.display())),
        }
    }
    Ok(())
}

/// Create `dir` (and parents) with mode 0o755, matching portage's empty
/// `${DIROPTIONS}` default. Every image-dir creation in this module goes
/// through here so modes are normalised to 0755 even when an ebuild tightens
/// the umask (`umask 077`) via the brush builtin.
fn mkdir_p(dir: &Path) -> Result<(), String> {
    mkdir_p_mode(dir, 0o755)
}

fn ok_status(
    st: std::io::Result<std::process::ExitStatus>,
    verb: &str,
    what: &Path,
) -> Result<(), String> {
    match st {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!("failed to {verb} {}", what.display())),
    }
}

fn install_file(
    env: &[(String, String)],
    opts: &[String],
    src: &Path,
    dest: &Path,
) -> Result<(), String> {
    let mut c = Command::new("install");
    for o in opts {
        if !o.is_empty() {
            c.arg(o);
        }
    }
    let st = c.arg(src).arg(dest).envs(env.iter().cloned()).status();
    ok_status(st, "install", src)
}

/// Install `src` into `destdir`, but if `src` is a symlink recreate it as a
/// symlink (`ln -s "$(readlink src)"`) rather than dereferencing and copying its
/// target. Mirrors portage's `dolib`, which preserves the `libfoo.so ->
/// libfoo.so.N` symlinks shipped alongside the real shared object.
fn install_or_symlink(
    env: &[(String, String)],
    opts: &[String],
    src: &Path,
    destdir: &Path,
) -> Result<(), String> {
    let dest = destdir.join(basename(src));
    if src.is_symlink() {
        let link =
            std::fs::read_link(src).map_err(|e| format!("reading link {}: {e}", src.display()))?;
        let st = Command::new("ln")
            .arg("-sf")
            .arg(&link)
            .arg(&dest)
            .envs(env.iter().cloned())
            .status();
        ok_status(st, "symlink", &dest)
    } else {
        install_file(env, opts, src, &dest)
    }
}

/// Recursively install the directory `src` under `dest_parent` (landing at
/// `dest_parent/<basename(src)>`), mirroring portage `doins --recursive`: each
/// subdirectory is created with mode 0755, each regular file is installed with
/// `opts` (so modes/owner are normalised, timestamps are not preserved), and
/// symlinks are recreated as symlinks rather than dereferenced.
fn install_tree(
    env: &[(String, String)],
    opts: &[String],
    src: &Path,
    dest_parent: &Path,
) -> Result<(), String> {
    let dest = dest_parent.join(basename(src));
    mkdir_p(&dest)?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("reading {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading {}: {e}", src.display()))?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        if ft.is_symlink() {
            let target = dest.join(entry.file_name());
            let link = std::fs::read_link(&path)
                .map_err(|e| format!("reading link {}: {e}", path.display()))?;
            let _ = std::fs::remove_file(&target);
            std::os::unix::fs::symlink(&link, &target)
                .map_err(|e| format!("symlink {}: {e}", target.display()))?;
        } else if ft.is_dir() {
            install_tree(env, opts, &path, &dest)?;
        } else {
            install_file(env, opts, &path, &dest.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn basename(p: &Path) -> std::ffi::OsString {
    p.file_name().map(|n| n.to_owned()).unwrap_or_default()
}

/// `os.path.relpath(target, dirname(link))` for the absolute paths `dosym -r`
/// uses (both rooted at `/`). Falls back to `target` verbatim if either is
/// relative (uncommon for `dosym -r`).
fn relpath(target: &str, link: &str) -> String {
    let start = link.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    if !target.starts_with('/') || !start.starts_with('/') {
        return target.to_string();
    }
    let t: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    let s: Vec<&str> = start.split('/').filter(|s| !s.is_empty()).collect();
    let common = t.iter().zip(s.iter()).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; s.len() - common];
    parts.extend(t[common..].iter().copied());
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// First-character man section for `name`, honouring a trailing compression
/// suffix (`Z`/`gz`/`bz2` are stripped before taking the section). Returns
/// `None` if the section isn't `[0-9n]`. Mirrors portage doman's
/// `man${suffix:0:1}` plus its `*man[0-9n]` validity check.
fn man_section(name: &str) -> Option<char> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let real = match base.rsplit_once('.') {
        Some((stem, "Z" | "gz" | "bz2")) => stem,
        _ => base,
    };
    let suffix = real.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let c = suffix.chars().next()?;
    (c.is_ascii_digit() || c == 'n').then_some(c)
}

/// Filename-based man locale routing: `name.LL.sect` / `name.LL_CC.sect` →
/// `(stripped_name, locale)`. Mirrors portage doman's `BASH_REMATCH` path,
/// used only when no explicit `-i18n=` was given.
fn man_locale(name: &str) -> Option<(String, String)> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let locale = parts[parts.len() - 2];
    let b = locale.as_bytes();
    let ok = (b.len() == 2 && b[0].is_ascii_lowercase() && b[1].is_ascii_lowercase())
        || (b.len() == 5
            && b[0].is_ascii_lowercase()
            && b[1].is_ascii_lowercase()
            && b[2] == b'_'
            && b[3].is_ascii_uppercase()
            && b[4].is_ascii_uppercase());
    if !ok {
        return None;
    }
    let sect = parts[parts.len() - 1];
    let base_name = parts[..parts.len() - 2].join(".");
    Some((format!("{base_name}.{sect}"), locale.to_string()))
}

macro_rules! require_files {
    ($ctx:expr, $files:expr, $helper:literal) => {
        if $files.is_empty() {
            return Ok(raise_die(
                &$ctx,
                concat!($helper, ": at least one argument required"),
            ));
        }
    };
}

// ---- destination-directory helpers ----

/// `dodir <dir>...` (PMS 12.3.1): create directories under `${ED}`
#[derive(Parser)]
pub(crate) struct DodirCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    dirs: Vec<String>,
}

impl builtins::Command for DodirCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.dirs, "dodir");
        let ed = ed(context.shell);
        let dirs = self.dirs.clone();
        Ok(run_blocking(&context, move || {
            for d in &dirs {
                mkdir_p(&under_ed(&ed, d)).map_err(|e| format!("dodir: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

/// `keepdir <dir>...` (PMS 12.3.1): `dodir` plus a `.keep_*` marker so the empty
/// directory survives image merging.
#[derive(Parser)]
pub(crate) struct KeepdirCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    dirs: Vec<String>,
}

impl builtins::Command for KeepdirCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.dirs, "keepdir");
        let ed = ed(context.shell);
        let category = var(context.shell, "CATEGORY", "");
        let pn = var(context.shell, "PN", "");
        // Marker uses the main slot only (`${SLOT%/*}`): drop any subslot.
        let slot = var(context.shell, "SLOT", "");
        let slot = slot.split('/').next().unwrap_or("").to_string();
        let dirs = self.dirs.clone();
        Ok(run_blocking(&context, move || {
            for d in &dirs {
                let dir = under_ed(&ed, d);
                mkdir_p(&dir).map_err(|e| format!("keepdir: {e}"))?;
                let keep = dir.join(format!(".keep_{category}_{pn}-{slot}"));
                std::fs::File::create(&keep)
                    .map_err(|e| format!("keepdir: creating {}: {e}", keep.display()))?;
            }
            Ok(())
        })
        .await)
    }
}

// ---- single-destination doers (install list of files into one dir) ----

/// Build the `doX`-style "install these files into `dest`" closure body shared by
/// the simple doers. `recursive` enables `-r` (copy directories wholesale).
fn install_files_closure(
    helper: &'static str,
    env: Vec<(String, String)>,
    opts: Vec<String>,
    dest: PathBuf,
    cwd: PathBuf,
    files: Vec<String>,
    recursive: bool,
) -> impl FnOnce() -> Result<(), String> + Send + 'static {
    move || {
        mkdir_p(&dest).map_err(|e| format!("{helper}: {e}"))?;
        for f in &files {
            let src = cwd.join(f);
            if recursive && src.is_dir() {
                install_tree(&env, &opts, &src, &dest).map_err(|e| format!("{helper}: {e}"))?;
            } else {
                let target = dest.join(basename(&src));
                install_file(&env, &opts, &src, &target).map_err(|e| format!("{helper}: {e}"))?;
            }
        }
        Ok(())
    }
}

/// `doins [-r] <file>...` (PMS 12.3.4): install into `${ED}${INSDESTTREE}` with
/// `INSOPTIONS` (default `-m0644`); `-r` recurses into directories.
#[derive(Parser)]
pub(crate) struct DoinsCommand {
    #[arg(short = 'r')]
    recursive: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoinsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doins");
        let env = super::context_env(&context);
        let insdest = var(context.shell, "INSDESTTREE", "");
        let opts: Vec<String> = var(context.shell, "_insopts", "-m0644")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let dest = under_ed(&ed(context.shell), &insdest);
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "doins",
                env,
                opts,
                dest,
                cwd,
                self.files.clone(),
                self.recursive,
            ),
        )
        .await)
    }
}

/// `doexe <file>...` (PMS 12.3.4): install into `${ED}${EXEDESTTREE}` with
/// `EXEOPTIONS` (default `-m0755`).
#[derive(Parser)]
pub(crate) struct DoexeCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoexeCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doexe");
        let env = super::context_env(&context);
        let exedest = var(context.shell, "EXEDESTTREE", "");
        let opts: Vec<String> = var(context.shell, "_exeopts", "-m0755")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let dest = under_ed(&ed(context.shell), &exedest);
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure("doexe", env, opts, dest, cwd, self.files.clone(), false),
        )
        .await)
    }
}

/// `dobin <file>...` (PMS 12.3.4): install executables into `${DESTTREE}/bin`
/// (mode 0755).
#[derive(Parser)]
pub(crate) struct DobinCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DobinCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dobin");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let dest = under_ed(&ed(context.shell), &format!("{into}/bin"));
        let cwd = context.shell.working_dir().to_path_buf();
        let mut opts = vec!["-m0755".to_string()];
        opts.extend(inst_owner_install_opts(&context));
        Ok(run_blocking(
            &context,
            install_files_closure("dobin", env, opts, dest, cwd, self.files.clone(), false),
        )
        .await)
    }
}

/// `dosbin <file>...` (PMS 12.3.4): install executables into `${DESTTREE}/sbin`
/// (mode 0755).
#[derive(Parser)]
pub(crate) struct DosbinCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DosbinCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dosbin");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let dest = under_ed(&ed(context.shell), &format!("{into}/sbin"));
        let cwd = context.shell.working_dir().to_path_buf();
        let mut opts = vec!["-m0755".to_string()];
        opts.extend(inst_owner_install_opts(&context));
        Ok(run_blocking(
            &context,
            install_files_closure("dosbin", env, opts, dest, cwd, self.files.clone(), false),
        )
        .await)
    }
}

/// `dodoc [-r] <file>...` (PMS 12.3.4): install docs into
/// `${ED}/usr/share/doc/${PF}[/${DOCDESTTREE}]` (mode 0644).
#[derive(Parser)]
pub(crate) struct DodocCommand {
    #[arg(short = 'r')]
    recursive: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DodocCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dodoc");
        let env = super::context_env(&context);
        let pf = var(context.shell, "PF", "");
        let docdesttree = var(context.shell, "DOCDESTTREE", "");
        let sub = if docdesttree.is_empty() {
            format!("usr/share/doc/{pf}")
        } else {
            format!("usr/share/doc/{pf}/{docdesttree}")
        };
        let dest = under_ed(&ed(context.shell), &sub);
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "dodoc",
                env,
                vec!["-m0644".into()],
                dest,
                cwd,
                self.files.clone(),
                self.recursive,
            ),
        )
        .await)
    }
}

/// `doheader [-r] <file>...` (PMS 12.3.4): install headers into
/// `${ED}/usr/include` (mode 0644).
#[derive(Parser)]
pub(crate) struct DoheaderCommand {
    #[arg(short = 'r')]
    recursive: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoheaderCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doheader");
        let env = super::context_env(&context);
        let dest = under_ed(&ed(context.shell), "usr/include");
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "doheader",
                env,
                vec!["-m0644".into()],
                dest,
                cwd,
                self.files.clone(),
                self.recursive,
            ),
        )
        .await)
    }
}

/// `doinfo <file>...` (PMS 12.3.4): install GNU info files into
/// `${ED}/usr/share/info` (mode 0644).
#[derive(Parser)]
pub(crate) struct DoinfoCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoinfoCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doinfo");
        let env = super::context_env(&context);
        let dest = under_ed(&ed(context.shell), "usr/share/info");
        let cwd = context.shell.working_dir().to_path_buf();
        Ok(run_blocking(
            &context,
            install_files_closure(
                "doinfo",
                env,
                vec!["-m0644".into()],
                dest,
                cwd,
                self.files.clone(),
                false,
            ),
        )
        .await)
    }
}

/// `doman [-i18n=<locale>] <file>...` (PMS 12.3.4): install man pages into
/// `${ED}/usr/share/man/man<section>`, section taken from each file's extension.
#[derive(Parser)]
pub(crate) struct DomanCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DomanCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "doman");
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let cwd = context.shell.working_dir().to_path_buf();
        let files = self.files.clone();
        Ok(run_blocking(&context, move || {
            let mut i18n = String::new();
            for f in &files {
                // `-i18n=<locale>` sets the locale prefix for following files
                // (empty resets it); `.keep_*` markers are silently ignored.
                if let Some(loc) = f.strip_prefix("-i18n=") {
                    i18n = if loc.is_empty() {
                        String::new()
                    } else {
                        format!("{loc}/")
                    };
                    continue;
                }
                let base = f.rsplit('/').next().unwrap_or(f).to_string();
                if base.starts_with(".keep_") {
                    continue;
                }
                let not_a_man = || format!("doman: '{f}' is probably not a man page");
                let (mandir, name) = if i18n.is_empty() {
                    if let Some((nm, locale)) = man_locale(&base) {
                        let sect = man_section(&nm).ok_or_else(not_a_man)?;
                        (format!("{locale}/man{sect}"), nm)
                    } else {
                        let sect = man_section(&base).ok_or_else(not_a_man)?;
                        (format!("man{sect}"), base)
                    }
                } else {
                    let sect = man_section(&base).ok_or_else(not_a_man)?;
                    (format!("{i18n}man{sect}"), base)
                };
                let src = cwd.join(f);
                // portage's `[[ -s ]]`: install only non-empty files; a missing
                // source is an error, an existing-but-empty one is skipped.
                match std::fs::metadata(&src) {
                    Ok(m) if m.len() > 0 => {
                        let dest = under_ed(&ed, &format!("usr/share/man/{mandir}"));
                        mkdir_p(&dest).map_err(|e| format!("doman: {e}"))?;
                        install_file(&env, &["-m0644".into()], &src, &dest.join(&name))
                            .map_err(|e| format!("doman: {e}"))?;
                    }
                    Ok(_) => {}
                    Err(_) => return Err(format!("doman: {f} does not exist")),
                }
            }
            Ok(())
        })
        .await)
    }
}

/// `domo <file>...` (PMS 12.3.4): install `.mo` files into
/// `${ED}/usr/share/locale/<locale>/LC_MESSAGES/${MOPREFIX:-${PN}}.mo`.
#[derive(Parser)]
pub(crate) struct DomoCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DomoCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "domo");
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let pn = var(context.shell, "PN", "");
        let moprefix = var(context.shell, "MOPREFIX", &pn);
        let cwd = context.shell.working_dir().to_path_buf();
        let files = self.files.clone();
        Ok(run_blocking(&context, move || {
            for f in &files {
                let src = cwd.join(f);
                let stem = Path::new(&basename(&src))
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let dest = under_ed(&ed, &format!("usr/share/locale/{stem}/LC_MESSAGES"));
                mkdir_p(&dest).map_err(|e| format!("domo: {e}"))?;
                install_file(
                    &env,
                    &["-m0644".into()],
                    &src,
                    &dest.join(format!("{moprefix}.mo")),
                )
                .map_err(|e| format!("domo: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

// ---- library helpers ----

/// `dolib.a <file>...` (PMS 12.3.4): install static libs into
/// `${DESTTREE}/$(get_libdir)` (mode 0644).
#[derive(Parser)]
pub(crate) struct DolibaCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DolibaCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dolib.a");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let libdir = get_libdir(context.shell);
        let dest = under_ed(&ed(context.shell), &format!("{into}/{libdir}"));
        let cwd = context.shell.working_dir().to_path_buf();
        let files = self.files.clone();
        Ok(run_blocking(&context, move || {
            mkdir_p(&dest).map_err(|e| format!("dolib.a: {e}"))?;
            for f in &files {
                install_or_symlink(&env, &["-m0644".into()], &cwd.join(f), &dest)
                    .map_err(|e| format!("dolib.a: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

/// `dolib.so <file>...` (PMS 12.3.4): install shared libs into
/// `${DESTTREE}/$(get_libdir)` (mode 0755).
#[derive(Parser)]
pub(crate) struct DolibsoCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DolibsoCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dolib.so");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let libdir = get_libdir(context.shell);
        let dest = under_ed(&ed(context.shell), &format!("{into}/{libdir}"));
        let cwd = context.shell.working_dir().to_path_buf();
        let files = self.files.clone();
        Ok(run_blocking(&context, move || {
            mkdir_p(&dest).map_err(|e| format!("dolib.so: {e}"))?;
            for f in &files {
                install_or_symlink(&env, &["-m0755".into()], &cwd.join(f), &dest)
                    .map_err(|e| format!("dolib.so: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

/// `dolib <file>...` (PMS 12.3.4): dispatch each file to `dolib.so`/`dolib.a` by
/// suffix (`.so`/`.so.*` are shared libraries, mode 0755; rest are 0644).
#[derive(Parser)]
pub(crate) struct DolibCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DolibCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        require_files!(context, self.files, "dolib");
        let env = super::context_env(&context);
        let into = var(context.shell, "_into_dir", "/usr");
        let libdir = get_libdir(context.shell);
        let dest = under_ed(&ed(context.shell), &format!("{into}/{libdir}"));
        let cwd = context.shell.working_dir().to_path_buf();
        let files = self.files.clone();
        Ok(run_blocking(&context, move || {
            mkdir_p(&dest).map_err(|e| format!("dolib: {e}"))?;
            for f in &files {
                let src = cwd.join(f);
                let name = basename(&src);
                let n = name.to_string_lossy();
                let mode = if n.ends_with(".so") || n.contains(".so.") {
                    "-m0755"
                } else {
                    "-m0644"
                };
                install_or_symlink(&env, &[mode.into()], &src, &dest)
                    .map_err(|e| format!("dolib: {e}"))?;
            }
            Ok(())
        })
        .await)
    }
}

// ---- symlinks, perms, ownership ----

/// `dosym [-r] <target> <link>` (PMS 12.3.4): create a symlink at `${ED}${link}`;
/// `-r` makes `target` relative to the link's directory.
#[derive(Parser)]
pub(crate) struct DosymCommand {
    #[arg(short = 'r')]
    relative: bool,
    #[arg(allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for DosymCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        if self.args.len() != 2 {
            return Ok(raise_die(&context, "dosym: usage: dosym [-r] target link"));
        }
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let target = self.args[0].clone();
        let link = self.args[1].clone();
        let relative = self.relative;
        // PMS forbids an implicit basename (bug #379899): reject a link that
        // ends in '/' or names an existing non-symlink directory.
        let link_path = under_ed(&ed, &link);
        if link.ends_with('/') || (link_path.is_dir() && !link_path.is_symlink()) {
            return Ok(raise_die(
                &context,
                &format!("dosym: link omits basename: '{link}'"),
            ));
        }
        Ok(run_blocking(&context, move || {
            let link_dir = link.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            mkdir_p(&under_ed(&ed, link_dir)).map_err(|e| format!("dosym: {e}"))?;
            let resolved = if relative {
                relpath(&target, &link)
            } else {
                target.clone()
            };
            let link_path = under_ed(&ed, &link);
            let st = Command::new("ln")
                .arg("-snf")
                .arg(&resolved)
                .arg(&link_path)
                .envs(env.iter().cloned())
                .status();
            ok_status(st, "symlink", &link_path).map_err(|e| format!("dosym: {e}"))
        })
        .await)
    }
}

/// `fperms <mode> <file>...` (PMS 12.3.5): `chmod` paths relative to `${ED}`
#[derive(Parser)]
pub(crate) struct FpermsCommand {
    #[arg(allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for FpermsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        if self.args.is_empty() {
            return Ok(raise_die(&context, "fperms: usage: fperms mode file..."));
        }
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let raw = self.args.clone();
        Ok(run_blocking(&context, move || {
            // Leading `-*` are options unless they're a bare mode char
            // (`-[ugorwxXst]`); the first non-option is the mode (unprefixed),
            // the rest are paths under ${ED}. Mirrors portage's fperms.
            let mut cmd_args: Vec<std::ffi::OsString> = Vec::new();
            let mut got_mode = false;
            for arg in &raw {
                let is_opt = arg.starts_with('-')
                    && !(arg.len() == 2 && b"ugorwxXst".contains(&arg.as_bytes()[1]));
                if is_opt || !got_mode {
                    got_mode |= !is_opt;
                    cmd_args.push(arg.into());
                } else {
                    cmd_args.push(under_ed(&ed, arg).into_os_string());
                }
            }
            let st = Command::new("chmod")
                .args(&cmd_args)
                .envs(env.iter().cloned())
                .status();
            match st {
                Ok(s) if s.success() => Ok(()),
                _ => Err("fperms failed".to_string()),
            }
        })
        .await)
    }
}

/// `fowners <owner>[:<group>] <file>...` (PMS 12.3.5): `chown` paths relative to
/// `${ED}`.
#[derive(Parser)]
pub(crate) struct FownersCommand {
    #[arg(allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for FownersCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        if self.args.is_empty() {
            return Ok(raise_die(&context, "fowners: usage: fowners owner file..."));
        }
        let env = super::context_env(&context);
        let ed = ed(context.shell);
        let raw = self.args.clone();
        // Resolve owner names against the *target* passwd/group, as portage does:
        // `<ESYSROOT|EROOT>/etc/{passwd,group}` during install. Gate on the target
        // root being an offset (non-`/`); for a host install the system chown
        // resolves against the same db, so the name is passed through.
        let pwdb_root = {
            let esysroot = var(context.shell, "ESYSROOT", "");
            if esysroot.is_empty() {
                var(context.shell, "EROOT", "")
            } else {
                esysroot
            }
        };
        let gate = {
            let sysroot = var(context.shell, "SYSROOT", "");
            let r = if sysroot.is_empty() {
                var(context.shell, "ROOT", "")
            } else {
                sysroot
            };
            !r.trim_end_matches('/').is_empty()
        };
        let pwdb_etc = PathBuf::from(pwdb_root.trim_end_matches('/')).join("etc");
        Ok(run_blocking(&context, move || {
            // Leading `-*` are options; the first non-option is the owner
            // (unprefixed, resolved to numeric uid:gid against the target db),
            // the rest are paths under ${ED}. Mirrors portage's fowners.
            let mut cmd_args: Vec<std::ffi::OsString> = Vec::new();
            let mut got_owner = false;
            for arg in &raw {
                if arg.starts_with('-') {
                    cmd_args.push(arg.into());
                } else if !got_owner {
                    got_owner = true;
                    let owner = if gate {
                        resolve_owner(arg, &pwdb_etc)?
                    } else {
                        arg.clone()
                    };
                    cmd_args.push(owner.into());
                } else {
                    cmd_args.push(under_ed(&ed, arg).into_os_string());
                }
            }
            let st = Command::new("chown")
                .args(&cmd_args)
                .envs(env.iter().cloned())
                .status();
            match st {
                Ok(s) if s.success() => Ok(()),
                // An unprivileged build cannot chown to root or a group it is not
                // in (`fowners :shadow`, `fowners root:portage` — pam, eselect):
                // chown returns EPERM. Portage treats this as non-fatal in
                // unprivileged mode (the file keeps the build user's ownership;
                // the real ids are applied by a privileged merge or a stage's tar
                // `--owner`). A privileged (root) build must still die — there a
                // chown failure is a genuine error.
                _ if !rustix::process::getuid().is_root() => {
                    tracing::info!(
                        "fowners: cannot set ownership '{}' (unprivileged build) — leaving as-is",
                        raw.first().map(String::as_str).unwrap_or("")
                    );
                    Ok(())
                }
                _ => Err("fowners failed".to_string()),
            }
        })
        .await)
    }
}

// ---- new* helpers ----

/// Which `new*` helper is running. Each mirrors its `do*` sibling's destination
/// tree and install mode; the only `new*`-specific behaviour is staging the
/// source under the requested name (and reading stdin when the source is `-`).
///
/// See [PMS 12.3.4](https://projects.gentoo.org/pms/9/pms.html#available-commands).
enum NewKind {
    Bin,
    Sbin,
    Ins,
    Exe,
    Doc,
    Man,
    Header,
    LibA,
    LibSo,
    Initd,
    Confd,
    Envd,
}

impl NewKind {
    /// Map a registered builtin name to its kind
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "newbin" => Self::Bin,
            "newsbin" => Self::Sbin,
            "newins" => Self::Ins,
            "newexe" => Self::Exe,
            "newdoc" => Self::Doc,
            "newman" => Self::Man,
            "newheader" => Self::Header,
            "newlib.a" => Self::LibA,
            "newlib.so" => Self::LibSo,
            "newinitd" => Self::Initd,
            "newconfd" => Self::Confd,
            "newenvd" => Self::Envd,
            _ => return None,
        })
    }

    /// `(dest_dir, install_opts)` for this helper, computed from the shell env
    /// `name` is the target filename — only `newman` inspects it (for the
    /// section, which it takes from the *name* since the source may be stdin).
    fn target<SE: brush_core::ShellExtensions>(
        &self,
        shell: &brush_core::Shell<SE>,
        name: &str,
    ) -> (PathBuf, Vec<String>) {
        let ed = ed(shell);
        let into = var(shell, "_into_dir", "/usr");
        match self {
            Self::Bin => (under_ed(&ed, &format!("{into}/bin")), vec!["-m0755".into()]),
            Self::Sbin => (
                under_ed(&ed, &format!("{into}/sbin")),
                vec!["-m0755".into()],
            ),
            Self::Ins => (
                under_ed(&ed, &var(shell, "INSDESTTREE", "")),
                opts_var(shell, "_insopts", "-m0644"),
            ),
            Self::Exe => (
                under_ed(&ed, &var(shell, "EXEDESTTREE", "")),
                opts_var(shell, "_exeopts", "-m0755"),
            ),
            Self::Doc => {
                let pf = var(shell, "PF", "");
                let dd = var(shell, "DOCDESTTREE", "");
                let sub = if dd.is_empty() {
                    format!("usr/share/doc/{pf}")
                } else {
                    format!("usr/share/doc/{pf}/{dd}")
                };
                (under_ed(&ed, &sub), vec!["-m0644".into()])
            }
            Self::Man => {
                let sect = man_section(name).map(|c| c.to_string()).unwrap_or_default();
                (
                    under_ed(&ed, &format!("usr/share/man/man{sect}")),
                    vec!["-m0644".into()],
                )
            }
            Self::Header => (under_ed(&ed, "usr/include"), vec!["-m0644".into()]),
            Self::LibA => {
                let libdir = get_libdir(shell);
                (
                    under_ed(&ed, &format!("{into}/{libdir}")),
                    vec!["-m0644".into()],
                )
            }
            Self::LibSo => {
                let libdir = get_libdir(shell);
                (
                    under_ed(&ed, &format!("{into}/{libdir}")),
                    vec!["-m0755".into()],
                )
            }
            Self::Initd => (under_ed(&ed, "etc/init.d"), vec!["-m0755".into()]),
            Self::Confd => (under_ed(&ed, "etc/conf.d"), vec!["-m0644".into()]),
            Self::Envd => (under_ed(&ed, "etc/env.d"), vec!["-m0644".into()]),
        }
    }
}

/// Split a whitespace-separated options var (`_insopts`/`_exeopts`) into args,
/// falling back to `default` when unset/empty.
fn opts_var<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    name: &str,
    default: &str,
) -> Vec<String> {
    var(shell, name, default)
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Shared `new* <src> <name>` parser (PMS 12.3.4). A single type is registered
/// under every `new*` name; [`builtins::Command::execute`] dispatches on
/// `context.command_name`. A literal `-` source reads the content from stdin
/// (PMS 12.3.x), staged under `${T}` before install so the file lands under the
/// requested name — e.g. `acct-group.eclass`'s `newins - foo.conf < <(…)`.
#[derive(Parser)]
pub(crate) struct NewCommand {
    #[arg(allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for NewCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let helper = context.command_name.clone();
        let Some(kind) = NewKind::from_name(&helper) else {
            return Ok(raise_die(&context, &format!("{helper}: not a new* helper")));
        };
        if self.args.len() != 2 {
            return Ok(raise_die(
                &context,
                &format!("{helper}: exactly two arguments required"),
            ));
        }
        let name = self.args[1].clone();

        // For newman the section comes from the *name* (the source may be
        // stdin and have no extension); validate it before any I/O, matching
        // doman's error.
        if matches!(kind, NewKind::Man) && man_section(&name).is_none() {
            return Ok(raise_die(
                &context,
                &format!("newman: '{name}' is probably not a man page"),
            ));
        }

        let env = super::context_env(&context);
        let (dest, mut opts) = kind.target(context.shell, &name);
        if matches!(kind, NewKind::Bin | NewKind::Sbin) {
            opts.extend(inst_owner_install_opts(&context));
        }
        let cwd = context.shell.working_dir().to_path_buf();
        let t = var(context.shell, "T", "");
        let t_dir = if t.is_empty() {
            std::env::temp_dir()
        } else {
            PathBuf::from(t)
        };
        let src = self.args[0].clone();

        // `-` = stdin: read it now (new* payloads are small — configs/scripts)
        // and stage under ${T}, so the blocking install closure has a plain
        // file to hand to install(1). Mirrors the old bash `cat > "${T}/$2"`.
        let stdin_buf: Option<Vec<u8>> = if src == "-" {
            let mut buf = Vec::new();
            match context.stdin().read_to_end(&mut buf) {
                Ok(_) => Some(buf),
                Err(e) => {
                    return Ok(raise_die(
                        &context,
                        &format!("{helper}: failed to read stdin: {e}"),
                    ));
                }
            }
        } else {
            None
        };

        Ok(run_blocking(&context, move || {
            mkdir_p(&dest).map_err(|e| format!("{helper}: {e}"))?;
            let target = dest.join(&name);
            if let Some(buf) = stdin_buf {
                let stage = t_dir.join(format!(".{name}.new-src"));
                let r = std::fs::write(&stage, buf).and_then(|()| {
                    install_file(&env, &opts, &stage, &target).map_err(std::io::Error::other)
                });
                let _ = std::fs::remove_file(&stage);
                r.map_err(|e| format!("{helper}: {e}"))
            } else {
                install_file(&env, &opts, &cwd.join(&src), &target)
                    .map_err(|e| format!("{helper}: {e}"))
            }
        })
        .await)
    }
}

/// do*/new* helper names backed by Rust builtins. `init_build_env` drops a tiny
/// PATH shim per name (each re-invoking `em __helper <name>`) so `find -exec
/// doman` / `xargs do*` work: `find` needs a real executable, which an in-shell
/// builtin is not. The bash-only `doinitd`/`doconfd`/`doenvd` wrappers are
/// intentionally excluded (no builtin to dispatch to, and `find -exec` on them
/// is effectively never used).
pub(crate) const HELPER_NAMES: &[&str] = &[
    "dodir",
    "keepdir",
    "doins",
    "doexe",
    "dobin",
    "dosbin",
    "dodoc",
    "doheader",
    "doinfo",
    "doman",
    "domo",
    "dolib",
    "dolib.a",
    "dolib.so",
    "dosym",
    "fperms",
    "fowners",
    "newbin",
    "newsbin",
    "newins",
    "newexe",
    "newdoc",
    "newman",
    "newheader",
    "newlib.a",
    "newlib.so",
    "newinitd",
    "newconfd",
    "newenvd",
];

/// Register every do*/new* install-helper builtin on `shell`. Shared by the
/// ebuild shell and the standalone `em __helper` runner so both use identical
/// logic. One `NewCommand` is registered under every `new*` name; it dispatches
/// on `context.command_name`.
pub(crate) fn register_install_builtins<SE: brush_core::ShellExtensions>(
    shell: &mut brush_core::Shell<SE>,
) {
    macro_rules! reg {
        ($($name:literal => $ty:ident),+ $(,)?) => {$(
            shell.register_builtin($name, builtins::builtin::<$ty, _>());
        )+};
    }
    reg! {
        "dodir" => DodirCommand,
        "keepdir" => KeepdirCommand,
        "doins" => DoinsCommand,
        "doexe" => DoexeCommand,
        "dobin" => DobinCommand,
        "dosbin" => DosbinCommand,
        "dodoc" => DodocCommand,
        "doheader" => DoheaderCommand,
        "doinfo" => DoinfoCommand,
        "doman" => DomanCommand,
        "domo" => DomoCommand,
        "dolib" => DolibCommand,
        "dolib.a" => DolibaCommand,
        "dolib.so" => DolibsoCommand,
        "dosym" => DosymCommand,
        "fperms" => FpermsCommand,
        "fowners" => FownersCommand,
        "newbin" => NewCommand,
        "newsbin" => NewCommand,
        "newins" => NewCommand,
        "newexe" => NewCommand,
        "newdoc" => NewCommand,
        "newman" => NewCommand,
        "newheader" => NewCommand,
        "newlib.a" => NewCommand,
        "newlib.so" => NewCommand,
        "newinitd" => NewCommand,
        "newconfd" => NewCommand,
        "newenvd" => NewCommand,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pwdb() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("etc")).unwrap();
        std::fs::write(
            dir.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/bash\nportage:x:250:250:portage:/var/tmp/portage:/sbin/nologin\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("etc/group"),
            "root:x:0:\nshadow:x:18:\nportage:x:250:\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn resolve_owner_matches_portage() {
        let dir = pwdb();
        let etc = dir.path().join("etc");
        // root:root -> 0:0 (numeric and name agree).
        assert_eq!(resolve_owner("root:root", &etc).unwrap(), "0:0");
        // name -> target numeric id (250, not whatever the host has).
        assert_eq!(resolve_owner("portage:portage", &etc).unwrap(), "250:250");
        // group-only name (`root:shadow`, pam/eselect) -> uid:gid.
        assert_eq!(resolve_owner("root:shadow", &etc).unwrap(), "0:18");
        // user only -> just uid.
        assert_eq!(resolve_owner("portage", &etc).unwrap(), "250");
        // `:group` -> chgrp form.
        assert_eq!(resolve_owner(":shadow", &etc).unwrap(), ":18");
        // numeric components pass through untouched.
        assert_eq!(resolve_owner("0:18", &etc).unwrap(), "0:18");
        // trailing `:` uses the user's primary group from passwd.
        assert_eq!(resolve_owner("portage:", &etc).unwrap(), "250:250");
    }

    #[test]
    fn resolve_owner_absent_name_is_error() {
        let dir = pwdb();
        let etc = dir.path().join("etc");
        assert!(resolve_owner("nobody:nogroup", &etc).is_err());
        assert!(resolve_owner("root:nogroup", &etc).is_err());
    }

    #[test]
    fn resolve_owner_empty_target_falls_back_to_host() {
        // Pre-baselayout ROOT: no passwd/group files → host /etc.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("etc")).unwrap();
        let etc = dir.path().join("etc");
        // Host always has root:0; portage is standard on Gentoo (this CI box).
        let got = resolve_owner("root:root", &etc).unwrap();
        assert_eq!(got, "0:0");
        // Name present only on host, not in empty target.
        if crate::userdb::id_in_file(Path::new("/etc/passwd"), "portage").is_some() {
            let got = resolve_owner("root:portage", &etc).unwrap();
            assert!(
                got.starts_with("0:"),
                "expected host-resolved root:portage, got {got}"
            );
        }
    }

    #[test]
    fn resolve_owner_populated_target_does_not_use_host() {
        // Target has a passwd but no "portage" — must not silently take host's.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("etc")).unwrap();
        std::fs::write(
            dir.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/bash\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("etc/group"), "root:x:0:\n").unwrap();
        let etc = dir.path().join("etc");
        assert!(resolve_owner("root:portage", &etc).is_err());
        assert_eq!(resolve_owner("root:root", &etc).unwrap(), "0:0");
    }

    #[test]
    fn man_section_takes_first_char_of_suffix() {
        // Plain single-char sections.
        assert_eq!(man_section("foo.1"), Some('1'));
        assert_eq!(man_section("foo.8"), Some('8'));
        assert_eq!(man_section("foo.n"), Some('n'));
        // Multi-char suffixes collapse to the first char (portage's
        // `${suffix:0:1}`): `.3pm` -> man3, not man3pm.
        assert_eq!(man_section("Foo.3pm"), Some('3'));
        assert_eq!(man_section("bar.1p"), Some('1'));
        // Compression suffixes are stripped before taking the section.
        assert_eq!(man_section("foo.1.gz"), Some('1'));
        assert_eq!(man_section("foo.3pm.bz2"), Some('3'));
        // Leading path is ignored.
        assert_eq!(man_section("a/b/foo.5"), Some('5'));
        // Not a man page: non-`[0-9n]` section, or no extension.
        assert_eq!(man_section("foo.txt"), None);
        assert_eq!(man_section("README"), None);
    }

    #[test]
    fn man_locale_detects_language_in_filename() {
        assert_eq!(
            man_locale("foo.de.1"),
            Some(("foo.1".to_string(), "de".to_string()))
        );
        assert_eq!(
            man_locale("foo.pt_BR.8"),
            Some(("foo.8".to_string(), "pt_BR".to_string()))
        );
        // Dotted basename keeps the leading components.
        assert_eq!(
            man_locale("foo.bar.de.1"),
            Some(("foo.bar.1".to_string(), "de".to_string()))
        );
        // Not a locale: wrong shape, uppercase lang, plain page.
        assert_eq!(man_locale("foo.1"), None);
        assert_eq!(man_locale("foo.DE.1"), None);
        assert_eq!(man_locale("foo.deu.1"), None);
    }

    #[test]
    fn relpath_matches_os_path_relpath() {
        // dosym -r: link path -> relative target from the link's dir.
        assert_eq!(relpath("/usr/bin/foo", "/usr/bin/bar"), "foo");
        assert_eq!(
            relpath("/usr/lib/libfoo.so", "/usr/bin/foo"),
            "../lib/libfoo.so"
        );
        // target is inside the link's own directory.
        assert_eq!(relpath("/a/b/c", "/a/b/link"), "c");
        // target equals the link's directory.
        assert_eq!(relpath("/a/b", "/a/b/link"), ".");
        assert_eq!(relpath("/bin/sh", "/usr/bin/sh"), "../../bin/sh");
        // Relative inputs are passed through verbatim.
        assert_eq!(relpath("../x", "y"), "../x");
    }

    #[test]
    fn mkdir_p_mode_applies_mode_to_each_created_component() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        // The requested mode lands on every directory we create along the way,
        // not just the leaf — matching `install -d -m<mode>`. Mode is set with
        // an explicit chmod, so it is independent of the process umask.
        let created = dir.path().join("a/b/c");
        mkdir_p_mode(&created, 0o755).unwrap();
        for part in ["a", "a/b", "a/b/c"] {
            assert_eq!(
                std::fs::metadata(dir.path().join(part)).unwrap().mode() & 0o777,
                0o755,
                "{part} not 0755"
            );
        }
    }

    #[test]
    fn mkdir_p_mode_leaves_preexisting_parents_untouched() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        // A parent created earlier with a custom mode must keep it — we only
        // chmod what we create (matching `install -d`). This is the one thing
        // a custom recursive mkdir does that `create_dir_all` cannot: an
        // absolute image path must never re-mode /usr and friends.
        let parent = dir.path().join("pre");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o750)).unwrap();
        mkdir_p_mode(&parent.join("child"), 0o755).unwrap();
        assert_eq!(
            std::fs::metadata(&parent).unwrap().mode() & 0o777,
            0o750,
            "pre-existing parent mode was clobbered"
        );
        assert_eq!(
            std::fs::metadata(parent.join("child")).unwrap().mode() & 0o777,
            0o755
        );
    }
}
