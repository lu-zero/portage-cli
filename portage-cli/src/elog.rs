//! Portage's elog system: what becomes of the messages an ebuild's `e*` calls
//! left behind in `${T}/logging/`.
//!
//! The producer is [`portage_repo`]'s `e*` builtins, which mirror every message
//! into `${T}/logging/${EBUILD_PHASE}` exactly as `isolated-functions.sh`'s
//! `__elog_base` does. This module is the consumer half — portage's
//! `portage/elog/`: collect those files, filter by `PORTAGE_ELOG_CLASSES`, and
//! hand the result to each module named in `PORTAGE_ELOG_SYSTEM`.
//!
//! The work is split across the two processes a merge runs in, following where
//! each half's inputs live:
//!
//! - **Collection and the file-writing modules** run at the end of the merge
//!   chain ([`crate::ebuild`]'s `run_inner`), which is the privilege-wrapped
//!   `em __worker` for a split build. That is the only side that can read
//!   `${T}` before the build tree is dropped, and the only one that reliably
//!   has permission to write under `<broot>/var/log/portage`.
//! - **`echo`** batches to the end of the whole `emerge` run, so the worker
//!   leaves its filtered text in `<work_dir>/elog` and the parent picks it up
//!   ([`take_pending`]) and prints it all at once ([`finalize_echo`]) — the same
//!   deferral real portage gets from `mod_echo`'s module-global `_items`.
//!
//! What gets *filed* is portage's own combined-log format, so `em read` and
//! real portage can read each other's files. The worker→parent handoff uses a
//! different, unambiguous one ([`PackageLog::to_handoff`]): portage's format
//! cannot be parsed back without guessing, and it only ever needs to be —
//! portage passes structured entries in-process and never round-trips them.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

use camino::{Utf8Path, Utf8PathBuf};

/// The elog message classes, portage's `_log_levels`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Info,
    Log,
    Warn,
    Error,
    Qa,
}

impl Class {
    const ALL: [Self; 5] = [Self::Info, Self::Log, Self::Warn, Self::Error, Self::Qa];

    /// The name used both in `${T}/logging/<phase>` and in the combined logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Log => "LOG",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Qa => "QA",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// The colour `echo` paints this class's marker with — the same palette the
    /// `e*` builtins used when the message was first printed, so a message
    /// looks identical whether it is seen live or replayed at the end.
    fn style(self) -> anstyle::Style {
        let p = crate::style::PORTAGE_COLORS;
        match self {
            Self::Info => p.info,
            Self::Log => p.log,
            Self::Warn => p.warn,
            Self::Error => p.err,
            Self::Qa => p.qawarn,
        }
    }
}

/// A set of [`Class`]es, as `PORTAGE_ELOG_CLASSES` and a module's `:`-suffix
/// both spell one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassSet(u8);

impl ClassSet {
    fn bit(class: Class) -> u8 {
        1 << Class::ALL.iter().position(|c| *c == class).unwrap_or(0)
    }

    /// Parse a class list. Names are case-insensitive and `*` means all of
    /// them, as `filter_loglevels` has it. Unknown names are ignored.
    pub fn parse(spec: &str) -> Self {
        let mut set = Self::default();
        for word in spec.split([' ', '\t', ',', '\n']).filter(|w| !w.is_empty()) {
            if word == "*" {
                return Self(u8::MAX);
            }
            if let Some(class) = Class::parse(&word.to_ascii_uppercase()) {
                set.0 |= Self::bit(class);
            }
        }
        set
    }

    pub fn contains(self, class: Class) -> bool {
        self.0 & Self::bit(class) != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Every class in either set.
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Phases in the order the combined log lists them, portage's
/// `EBUILD_PHASES`. Also the set of legal `${T}/logging/` file names — the
/// producer writes `${EBUILD_PHASE:-other}`, so anything else in there was put
/// there by something that is not an `e*` call.
const EBUILD_PHASES: &[&str] = &[
    "pretend",
    "setup",
    "unpack",
    "prepare",
    "configure",
    "compile",
    "test",
    "install",
    "package",
    "instprep",
    "preinst",
    "postinst",
    "prerm",
    "postrm",
    "nofetch",
    "config",
    "info",
    "other",
];

/// One package's messages, grouped by phase in [`EBUILD_PHASES`] order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageLog {
    phases: Vec<(&'static str, Vec<(Class, String)>)>,
}

impl PackageLog {
    pub fn is_empty(&self) -> bool {
        self.phases.is_empty()
    }

    fn push(&mut self, phase: &'static str, class: Class, message: String) {
        match self.phases.iter_mut().find(|(p, _)| *p == phase) {
            Some((_, entries)) => entries.push((class, message)),
            None => self.phases.push((phase, vec![(class, message)])),
        }
    }

    fn sort_phases(&mut self) {
        self.phases
            .sort_by_key(|(phase, _)| EBUILD_PHASES.iter().position(|p| p == phase).unwrap_or(0));
    }

    /// Read `${T}/logging/` and consume it, portage's
    /// `collect_ebuild_messages`.
    ///
    /// A missing directory is simply an empty log — the phase may never have
    /// run, or nothing may have called an `e*` function.
    ///
    /// Each file is unlinked once read ("clean logfiles to avoid repetitions",
    /// `messages.py`). It matters because the producer *appends*: under
    /// `FEATURES=keeptemp`/`keepwork` the build tree survives the merge, so a
    /// second merge of the same package would otherwise file every message from
    /// the first one again.
    pub fn collect(logging_dir: &Utf8Path) -> Self {
        let mut log = Self::default();
        let Ok(dir) = std::fs::read_dir(logging_dir.as_std_path()) else {
            return log;
        };
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(phase) = EBUILD_PHASES.iter().find(|p| **p == name) else {
                crate::style::warn_line!("elog: ignoring unknown log file {name}");
                continue;
            };
            let path = logging_dir.join(&name);
            let Ok(text) = std::fs::read_to_string(path.as_std_path()) else {
                crate::style::warn_line!("elog: cannot read {path}");
                continue;
            };
            let _ = std::fs::remove_file(path.as_std_path());
            // `split('\n')` rather than line iteration: a lone `\r` inside a
            // message is content, not a line break (portage bug #390833).
            for line in text.split('\n').filter(|l| !l.is_empty()) {
                match line
                    .split_once(' ')
                    .and_then(|(class, msg)| Some((Class::parse(class)?, msg)))
                {
                    Some((class, msg)) => log.push(phase, class, msg.to_string()),
                    None => crate::style::warn_line!("elog: malformed entry in {path}: {line}"),
                }
            }
        }
        log.sort_phases();
        log
    }

    /// Drop everything outside `classes`, portage's `filter_loglevels`.
    pub fn filter(&self, classes: ClassSet) -> Self {
        let phases = self
            .phases
            .iter()
            .filter_map(|(phase, entries)| {
                let kept: Vec<_> = entries
                    .iter()
                    .filter(|(class, _)| classes.contains(*class))
                    .cloned()
                    .collect();
                (!kept.is_empty()).then_some((*phase, kept))
            })
            .collect();
        Self { phases }
    }

    /// The combined-log text, portage's `_combine_logentries`: a
    /// `<CLASS>: <phase>` header whenever the class changes, then the messages
    /// under it. This is what lands in `/var/log/portage/elog/`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (phase, entries) in &self.phases {
            let mut previous = None;
            for (class, message) in entries {
                if previous != Some(*class) {
                    previous = Some(*class);
                    out.push_str(class.as_str());
                    out.push_str(": ");
                    out.push_str(phase);
                    out.push('\n');
                }
                out.push_str(message.trim_end_matches('\n'));
                out.push('\n');
            }
        }
        out
    }

    /// The worker→parent handoff format: one entry per line, `<CLASS> <phase>
    /// <message>`.
    ///
    /// Deliberately *not* [`to_text`](Self::to_text). That format is portage's
    /// and has to stay the one that gets filed, but it cannot be parsed back
    /// without guessing: a message reading `ERROR: setup` is indistinguishable
    /// from a header, and guessing wrong drops the message and re-colours the
    /// ones after it. Here the class and phase are fixed-vocabulary tokens
    /// split from the left, so the message keeps whatever it says. Entries are
    /// always single lines — the producer splits on `\n` before writing.
    fn to_handoff(&self) -> String {
        let mut out = String::new();
        for (phase, entries) in &self.phases {
            for (class, message) in entries {
                out.push_str(class.as_str());
                out.push(' ');
                out.push_str(phase);
                out.push(' ');
                out.push_str(message);
                out.push('\n');
            }
        }
        out
    }

    /// Inverse of [`to_handoff`](Self::to_handoff).
    fn from_handoff(text: &str) -> Self {
        let mut log = Self::default();
        // `split('\n')` not `lines()`: a trailing `\r` is content, not a line
        // break (portage bug #390833) — same contract as [`collect`](Self::collect),
        // so the round trip stays byte-faithful instead of dropping the `\r`
        // every `echo` replay would otherwise lose.
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            let parsed = line.split_once(' ').and_then(|(class, rest)| {
                let (phase, message) = rest.split_once(' ')?;
                let phase = EBUILD_PHASES.iter().find(|p| **p == phase)?;
                Some((Class::parse(class)?, *phase, message))
            });
            if let Some((class, phase, message)) = parsed {
                log.push(phase, class, message.to_string());
            }
        }
        log.sort_phases();
        log
    }

    /// Inverse of [`to_text`](Self::to_text) — how `em read` gets structure
    /// back out of a combined log on disk.
    ///
    /// Portage's format is ambiguous by construction, so this is best-effort in
    /// the same way `elogv` is: a line counts as a header only when its phase
    /// is a real one. Never used for `em`'s own handoff — see
    /// [`from_handoff`](Self::from_handoff).
    pub fn from_text(text: &str) -> Self {
        let mut log = Self::default();
        let mut current: Option<(&'static str, Class)> = None;
        for line in text.lines() {
            let header = line.split_once(": ").and_then(|(class, phase)| {
                let phase = EBUILD_PHASES.iter().find(|p| **p == phase)?;
                Some((*phase, Class::parse(class)?))
            });
            match (header, current) {
                (Some(h), _) => current = Some(h),
                (None, Some((phase, class))) if !line.is_empty() => {
                    log.push(phase, class, line.to_string());
                }
                _ => {}
            }
        }
        log.sort_phases();
        log
    }

    /// Render as the `e*` builtins would have, for `echo` and `em read`.
    fn print_to(&self, out: &mut impl Write) {
        for (_, entries) in &self.phases {
            for (class, message) in entries {
                let s = class.style();
                let _ = writeln!(out, " {s}*{s:#} {message}");
            }
        }
    }
}

/// An elog dispatch module — portage's `PORTAGE_ELOG_SYSTEM` names, minus the
/// ones `em` has no equivalent for (`mail`, `mail_summary`, `syslog`,
/// `custom`), which are parsed and ignored exactly as portage ignores a module
/// it cannot import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Module {
    /// One file per package: `<logdir>/elog/<cat>:<pf>:<timestamp>.log`.
    Save,
    /// Appended to `<logdir>/elog/summary.log`.
    SaveSummary,
    /// Replayed to the console when the whole run finishes.
    Echo,
}

impl Module {
    fn parse(name: &str) -> Option<Self> {
        // `-` is nicer than `_` in a make.conf, and portage accepts both.
        match name.replace('-', "_").as_str() {
            "save" => Some(Self::Save),
            "save_summary" => Some(Self::SaveSummary),
            "echo" => Some(Self::Echo),
            _ => None,
        }
    }
}

/// Resolved elog configuration for one merge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// Which modules to run, each with the classes it wants — its own
    /// `module:classes` override when given, otherwise `PORTAGE_ELOG_CLASSES`.
    pub systems: Vec<(Module, ClassSet)>,
    /// `<logdir>/elog/` is where the file-writing modules put their output.
    pub logdir: Utf8PathBuf,
}

impl Config {
    /// Build from the three `PORTAGE_ELOG_*` settings and the log directory,
    /// dropping modules that would have nothing to write.
    pub fn new(classes: &str, systems: &str, logdir: Utf8PathBuf) -> Self {
        let default_classes = ClassSet::parse(classes);
        let mut resolved: Vec<(Module, ClassSet)> = Vec::new();
        for spec in systems.split_whitespace() {
            let (name, classes) = match spec.split_once(':') {
                Some((name, list)) => (name, ClassSet::parse(list)),
                None => (spec, default_classes),
            };
            let Some(module) = Module::parse(name) else {
                continue;
            };
            // A module named twice gets the union of its class lists, as
            // portage's `levels_set.update` gives it — not the first mention.
            match resolved.iter_mut().find(|(m, _)| *m == module) {
                Some((_, existing)) => *existing = existing.union(classes),
                None => resolved.push((module, classes)),
            }
        }
        resolved.retain(|(_, classes)| !classes.is_empty());
        Self {
            systems: resolved,
            logdir,
        }
    }

    fn wants(&self, module: Module) -> Option<ClassSet> {
        self.systems
            .iter()
            .find(|(m, _)| *m == module)
            .map(|(_, c)| *c)
    }

    pub fn is_enabled(&self) -> bool {
        !self.systems.is_empty()
    }
}

/// Where the parent finds what the merge chain left for `echo`, under the
/// package's work dir (alongside `build.log`, which the tree drop also keeps).
pub const PENDING_ECHO_FILE: &str = "elog";

/// Where the `echo` module's share of a package's messages goes.
pub enum Echo<'a> {
    /// Leave it in `<work_dir>/elog` for the parent process to collect. The
    /// merge chain's own dispatch runs in the `em __worker` child, which is not
    /// the process that prints the end-of-run block.
    Handoff(&'a Utf8Path),
    /// Queue it here. For work with no `__worker` seam to cross — `-C` and the
    /// `pkg_prerm`/`pkg_postrm` of a package being replaced — whichever process
    /// dispatched is one that also calls [`finalize_echo`]. `root` is the tree
    /// the package was removed from, for the header.
    InProcess { root: Option<&'a Utf8Path> },
}

/// Run the file-writing modules for one package's messages, and hand the
/// `echo` module's share to `echo`.
///
/// Never fails the merge: a package that built and installed correctly must not
/// be reported as broken because its log could not be filed. Problems are
/// warned about instead.
pub fn dispatch(config: &Config, cpv: &portage_atom::Cpv, log: &PackageLog, echo: Echo<'_>) {
    if log.is_empty() || !config.is_enabled() {
        return;
    }
    let elogdir = config.logdir.join("elog");

    if let Some(classes) = config.wants(Module::Save) {
        let filtered = log.filter(classes);
        if !filtered.is_empty() {
            let name = format!("{}:{}:{}.log", cpv.cpn.category, pf(cpv), timestamp());
            write_elog_file(&elogdir, &name, &filtered.to_text(), false);
        }
    }

    if let Some(classes) = config.wants(Module::SaveSummary) {
        let filtered = log.filter(classes);
        if !filtered.is_empty() {
            let body = format!(
                ">>> Messages generated by process {} on {} for package {cpv}:\n\n{}\n",
                std::process::id(),
                timestamp_human(),
                filtered.to_text()
            );
            write_elog_file(&elogdir, "summary.log", &body, true);
        }
    }

    if let Some(classes) = config.wants(Module::Echo) {
        let filtered = log.filter(classes);
        if filtered.is_empty() {
            return;
        }
        match echo {
            Echo::Handoff(work_dir) => {
                let path = work_dir.join(PENDING_ECHO_FILE);
                if let Err(e) = std::fs::write(path.as_std_path(), filtered.to_handoff()) {
                    crate::style::warn_line!("elog: cannot write {path}: {e}");
                }
            }
            Echo::InProcess { root } => {
                queue_echo(cpv.to_string(), root.map(Utf8Path::to_string), filtered);
            }
        }
    }
}

/// `<pn>-<version>`, the `PF` half of an elog file name.
fn pf(cpv: &portage_atom::Cpv) -> String {
    format!("{}-{}", cpv.cpn.package, cpv.version)
}

/// Create the elog directory group-owned and setgid, portage's `mod_save`.
///
/// The point is the *directory*: mode 2770 lets any member of the group create
/// and remove entries in it whoever owns them, and the setgid bit puts every
/// file that lands there in the same group. That is what keeps logs written by
/// a privileged run (`--privilege sudo`, where the worker is real root)
/// manageable afterwards by the unprivileged user who ran `em`. Every other
/// backend already writes as that user — `pseudoroot` and `fakeroost` only fake
/// `chown`, and hakoniwa maps the container's root back to the caller — so this
/// is specifically the `sudo` case.
///
/// The group is the *invoking user's*, not `portage`. Naming portage's group
/// looks like the faithful choice and is the wrong one here: under
/// `--privilege sudo` that `chown` succeeds (root may give a directory to any
/// group), leaving `portage:2770` — and a user who is not in the portage group,
/// which is the usual case, still cannot manage their own logs. `SUDO_GID` is
/// the group of whoever actually ran `em`, so it always can. Without it the
/// creator's own group is already that answer and nothing needs changing.
///
/// This costs nothing against real portage's convention, because of the next
/// paragraph: a system where portage already made `/var/log/portage`
/// `portage:portage` keeps it, and `em`'s files land in it setgid-inherited
/// into the portage group exactly as portage's own do.
///
/// **Only on creation.** An existing directory's permissions belong to the
/// administrator (portage's own code says as much), and on a real Gentoo system
/// portage created this one long ago — so on the common path none of this runs.
fn ensure_elogdir(elogdir: &Utf8Path) -> std::io::Result<()> {
    if elogdir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(elogdir.as_std_path())?;

    // Mode first, and unconditionally: the setgid bit is what makes every file
    // written here — by any later run, under any backend — inherit the
    // directory's group instead of the writer's.
    let _ = rustix::fs::chmod(
        elogdir.as_std_path(),
        rustix::fs::Mode::from_bits_truncate(0o2770),
    );
    if let Some(gid) = std::env::var("SUDO_GID").ok().and_then(|g| g.parse().ok()) {
        let _ = rustix::fs::chown(
            elogdir.as_std_path(),
            None,
            Some(rustix::fs::Gid::from_raw(gid)),
        );
    }
    Ok(())
}

fn write_elog_file(elogdir: &Utf8Path, name: &str, body: &str, append: bool) {
    if let Err(e) = ensure_elogdir(elogdir) {
        crate::style::warn_line!("elog: cannot create {elogdir}: {e}");
        return;
    }
    let path = elogdir.join(name);
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(append)
        .write(!append)
        .truncate(!append)
        .open(path.as_std_path());
    match opened {
        Ok(mut file) => {
            // Group-readable, matching `ensure_elogdir`'s setgid intent: a log
            // filed under `sudo` lands `root:<invoker group>` and must stay
            // openable by that group for `em read`, which the process umask
            // (0077 under strict sudo) would otherwise deny.
            let _ = rustix::fs::fchmod(&file, rustix::fs::Mode::from_bits_truncate(0o660));
            if let Err(e) = file.write_all(body.as_bytes()) {
                crate::style::warn_line!("elog: cannot write {path}: {e}");
            }
        }
        Err(e) => crate::style::warn_line!("elog: cannot open {path}: {e}"),
    }
}

/// `%Y%m%d-%H%M%S` UTC, portage's elog file-name stamp, derived from an RFC 3339
/// rendering so no date library is needed for it.
fn timestamp() -> String {
    let s = timestamp_human();
    let (date, time) = match s.split_once('T') {
        Some((date, time)) => (date, time.trim_end_matches('Z')),
        None => return s,
    };
    format!("{}-{}", date.replace('-', ""), time.replace(':', ""))
}

/// RFC 3339 UTC. Portage's summary header uses local time with a `%Z` suffix;
/// `em` records UTC instead, which needs no timezone database and stays
/// sortable.
fn timestamp_human() -> String {
    humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string()
}

/// Messages waiting to be echoed once the run finishes — portage's `mod_echo`
/// module-global `_items`, which is likewise per-process and drained by a
/// single `finalize` at the end.
static PENDING: OnceLock<Mutex<Vec<EchoItem>>> = OnceLock::new();

/// One package's held messages, with the tree it was merged to or removed from.
struct EchoItem {
    package: String,
    root: Option<String>,
    log: PackageLog,
}

fn pending() -> &'static Mutex<Vec<EchoItem>> {
    PENDING.get_or_init(Mutex::default)
}

/// Hold one package's messages for [`finalize_echo`].
fn queue_echo(package: String, root: Option<String>, log: PackageLog) {
    if let Ok(mut queue) = pending().lock() {
        queue.push(EchoItem { package, root, log });
    }
}

/// Discard any handoff left in `<work_dir>/elog` by an earlier run.
///
/// The file sits at the work dir's top level, which the pre-merge clean leaves
/// alone (like `build.log`), so a run killed between the worker writing it and
/// the parent collecting it would otherwise have its messages replayed as if
/// they belonged to the next merge of that package.
pub fn discard_stale_pending(work_dir: &Utf8Path) {
    let _ = std::fs::remove_file(work_dir.join(PENDING_ECHO_FILE).as_std_path());
}

/// Take what the merge chain left in `<work_dir>/elog` and hold it for
/// [`finalize_echo`]. Called by the parent once a package has merged, whether
/// or not the chain ran in a `__worker` child.
pub fn take_pending(cpv: &portage_atom::Cpv, work_dir: &Utf8Path, root: &Utf8Path) {
    let path = work_dir.join(PENDING_ECHO_FILE);
    let Ok(text) = std::fs::read_to_string(path.as_std_path()) else {
        return;
    };
    let _ = std::fs::remove_file(path.as_std_path());
    let log = PackageLog::from_handoff(&text);
    if !log.is_empty() {
        queue_echo(cpv.to_string(), Some(root.to_string()), log);
    }
}

/// Replay every held message, portage's `mod_echo.finalize` — the
/// "Messages for package …" block at the end of an `emerge`.
///
/// Everything goes to stdout, including the warnings and errors, exactly as
/// `mod_echo` does it (it redirects stderr to stdout for the duration): the
/// block is one contiguous report, not something to be split across two
/// streams by class.
pub fn finalize_echo() {
    let Ok(mut queue) = pending().lock() else {
        return;
    };
    let items = std::mem::take(&mut *queue);
    drop(queue);

    let mut out = anstream::stdout();
    for item in &items {
        print_block(&mut out, &item.package, item.root.as_deref(), &item.log);
    }
    let _ = out.flush();
}

/// One package's block as `echo` renders it: a blank line, the header, a blank
/// line, then the messages. Shared with `em read`, which shows a saved log in
/// the same shape the live run would have.
fn print_block(out: &mut impl Write, package: &str, root: Option<&str>, log: &PackageLog) {
    let info = crate::style::PORTAGE_COLORS.info;
    let pkg = crate::style::C_PKG;
    let _ = writeln!(out);
    // Which tree the messages are about, when it is not the running system —
    // `mod_echo`'s "merged to %(root)s". `em` is offset-rooted far more often
    // than portage is, so this is the difference between a useful report and an
    // ambiguous one.
    match root.filter(|r| *r != "/") {
        Some(root) => {
            let _ = writeln!(
                out,
                " {info}*{info:#} Messages for package {pkg}{package}{pkg:#} merged to {root}:"
            );
        }
        None => {
            let _ = writeln!(
                out,
                " {info}*{info:#} Messages for package {pkg}{package}{pkg:#}:"
            );
        }
    }
    let _ = writeln!(out);
    log.print_to(out);
}

// ── Reading back what was saved (`em read`) ───────────────────────────────

/// A per-package file the `save` module wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedLog {
    pub path: Utf8PathBuf,
    /// `<category>/<pf>`, recovered from the file name.
    pub package: String,
    /// The file name's `%Y%m%d-%H%M%S` stamp, which is also its sort key.
    pub stamp: String,
}

impl SavedLog {
    /// The stamp as a human reads it. Purely presentational — the stamp itself
    /// stays the sort key, since it is already lexicographically ordered.
    pub fn when(&self) -> String {
        // The stamp comes from a file name, so it is whatever is on disk, not
        // necessarily what `save` wrote: byte-slicing it needs the digits
        // checked, or a multi-byte character at the wrong offset panics on a
        // char boundary and takes `em read` down with it.
        let (date, time) = match self.stamp.split_once('-') {
            Some((date, time))
                if date.len() == 8
                    && time.len() == 6
                    && date.bytes().chain(time.bytes()).all(|b| b.is_ascii_digit()) =>
            {
                (date, time)
            }
            _ => return self.stamp.clone(),
        };
        format!(
            "{}-{}-{} {}:{}:{}",
            &date[..4],
            &date[4..6],
            &date[6..],
            &time[..2],
            &time[2..4],
            &time[4..]
        )
    }
}

/// Every per-package file under `<logdir>/elog`, newest first.
///
/// Both layouts portage writes are read: the flat
/// `<category>:<pf>:<stamp>.log`, and `FEATURES=split-elog`'s
/// `<category>/<pf>:<stamp>.log`. `summary.log` is skipped — it is an
/// append-only digest of the same messages, not a per-package file.
pub fn saved_logs(elogdir: &Utf8Path) -> Vec<SavedLog> {
    fn parse(path: Utf8PathBuf, category: Option<&str>) -> Option<SavedLog> {
        let name = path.file_name()?.strip_suffix(".log")?;
        let mut parts = name.split(':');
        let (category, pf) = match category {
            // split-elog: the category is the directory, so the name is
            // `<pf>:<stamp>` and a `:` in it would be neither.
            Some(category) => (category.to_string(), parts.next()?.to_string()),
            None => (parts.next()?.to_string(), parts.next()?.to_string()),
        };
        let stamp = parts.next()?.to_string();
        (parts.next().is_none()).then(|| SavedLog {
            path,
            package: format!("{category}/{pf}"),
            stamp,
        })
    }

    let mut logs = Vec::new();
    let Ok(dir) = std::fs::read_dir(elogdir.as_std_path()) else {
        return logs;
    };
    for entry in dir.flatten() {
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            let category = path.file_name().map(str::to_string);
            let Ok(inner) = std::fs::read_dir(path.as_std_path()) else {
                continue;
            };
            logs.extend(inner.flatten().filter_map(|e| {
                let path = Utf8PathBuf::from_path_buf(e.path()).ok()?;
                parse(path, category.as_deref())
            }));
        } else if let Some(log) = parse(path, None) {
            logs.push(log);
        }
    }
    logs.sort_by(|a, b| {
        b.stamp
            .cmp(&a.stamp)
            .then_with(|| a.package.cmp(&b.package))
    });
    logs
}

/// `<logdir>/elog`: `PORTAGE_LOGDIR` from the environment, then from the
/// root-aware make.conf, then `<broot>/var/log/portage` — the same chain
/// [`crate::ebuild`]'s writer side resolves, so a reader started with the same
/// `--root`/`--config-root` looks where the merge actually wrote.
async fn elogdir(globals: &crate::cli::Cli) -> Utf8PathBuf {
    let roots = globals.roots();
    // Stop at the first layer that *sets* the variable, empty or not — the
    // same rule the writer side uses (`ebuild.rs`'s `elog_setting`), so both
    // halves look in the same directory for any given configuration.
    let configured = match std::env::var("PORTAGE_LOGDIR") {
        Ok(dir) => Some(dir),
        Err(_) => crate::binpkg::read_make_conf_var_for_roots(&roots, "PORTAGE_LOGDIR").await,
    };
    let logdir = match configured.map(|d| d.trim().to_string()) {
        Some(dir) if !dir.is_empty() => Utf8PathBuf::from(dir),
        _ => roots
            .broot()
            .unwrap_or_else(|| Utf8Path::new("/"))
            .join("var/log/portage"),
    };
    logdir.join("elog")
}

/// `em read` — show what the `save` module filed, the job `elogv` does
/// interactively.
pub async fn run_read(
    globals: &crate::cli::Cli,
    package: Option<&str>,
    list: bool,
    limit: usize,
    delete: bool,
) -> anyhow::Result<()> {
    let elogdir = elogdir(globals).await;
    let mut logs = saved_logs(&elogdir);
    if let Some(filter) = package {
        logs.retain(|log| log.package.contains(filter));
    }
    if logs.is_empty() {
        // Nothing filed is a normal state, not a failure — but say which
        // directory was looked in, since a wrong `--root` looks identical.
        match package {
            Some(filter) => println!("no elog messages for {filter} in {elogdir}"),
            None => println!("no elog messages in {elogdir}"),
        }
        return Ok(());
    }
    let shown = if limit == 0 { logs.len() } else { limit };
    let hidden = logs.len().saturating_sub(shown);
    logs.truncate(shown);

    let mut out = anstream::stdout();
    for log in &logs {
        if list {
            let _ = writeln!(out, "{}  {}", log.when(), log.package);
            continue;
        }
        match std::fs::read_to_string(log.path.as_std_path()) {
            Ok(text) => print_block(&mut out, &log.package, None, &PackageLog::from_text(&text)),
            Err(e) => crate::style::warn_line!("cannot read {}: {e}", log.path),
        }
    }
    if hidden > 0 {
        let _ = writeln!(out, "\n({hidden} older package(s) not shown; -n0 for all)");
    }
    let _ = out.flush();

    if delete {
        for log in &logs {
            if let Err(e) = std::fs::remove_file(log.path.as_std_path()) {
                crate::style::warn_line!("cannot remove {}: {e}", log.path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_of(entries: &[(&'static str, Class, &str)]) -> PackageLog {
        let mut log = PackageLog::default();
        for (phase, class, msg) in entries {
            log.push(phase, *class, (*msg).to_string());
        }
        log.sort_phases();
        log
    }

    #[test]
    fn classes_parse_like_filter_loglevels() {
        let set = ClassSet::parse("log warn error");
        assert!(
            set.contains(Class::Log) && set.contains(Class::Warn) && set.contains(Class::Error)
        );
        assert!(!set.contains(Class::Info) && !set.contains(Class::Qa));
        // A module override spells its list with commas, and `*` is all of them.
        assert_eq!(ClassSet::parse("log,warn,error"), set);
        for class in Class::ALL {
            assert!(ClassSet::parse("*").contains(class));
        }
        // Unknown names are ignored rather than rejected.
        assert!(ClassSet::parse("nosuchclass").is_empty());
    }

    #[test]
    fn collect_reads_the_producer_format() {
        let dir = tempfile::tempdir().unwrap();
        let logging = camino::Utf8Path::from_path(dir.path()).unwrap().to_owned();
        std::fs::write(logging.join("postinst"), "LOG one\nWARN two\nLOG three\n").unwrap();
        std::fs::write(logging.join("setup"), "INFO early\n").unwrap();

        let log = PackageLog::collect(&logging);
        // Phases come back in EBUILD_PHASES order regardless of readdir order.
        assert_eq!(
            log,
            log_of(&[
                ("setup", Class::Info, "early"),
                ("postinst", Class::Log, "one"),
                ("postinst", Class::Warn, "two"),
                ("postinst", Class::Log, "three"),
            ])
        );
    }

    #[test]
    fn combined_log_round_trips() {
        let log = log_of(&[
            ("setup", Class::Info, "early"),
            ("postinst", Class::Log, "one"),
            ("postinst", Class::Log, "two"),
            ("postinst", Class::Warn, "careful"),
        ]);
        // A header only when the class changes, as `_combine_logentries` has it.
        let text = log.to_text();
        assert_eq!(
            text,
            "INFO: setup\nearly\nLOG: postinst\none\ntwo\nWARN: postinst\ncareful\n"
        );
        assert_eq!(PackageLog::from_text(&text), log);

        // An empty log has no trailing blank line to strip.
        assert_eq!(PackageLog::default().to_text(), "");
    }

    /// The handoff must survive a message that reads exactly like a
    /// combined-log header for a real phase. Through `to_text` this one is
    /// destroyed — dropped, and the entries after it re-attributed to the class
    /// and phase it appears to announce.
    #[test]
    fn the_handoff_survives_a_message_that_looks_like_a_header() {
        let log = log_of(&[
            ("postinst", Class::Log, "first line"),
            ("postinst", Class::Log, "ERROR: setup"),
            ("postinst", Class::Log, "after the lookalike"),
        ]);
        assert_eq!(PackageLog::from_handoff(&log.to_handoff()), log);
        // Pin what the combined format does with it, so the reason the handoff
        // is separate does not get lost.
        assert_ne!(PackageLog::from_text(&log.to_text()), log);
    }

    /// A trailing `\r` inside a message is content, not a line break (portage
    /// bug #390833). `collect` keeps it via `split('\n')`; `from_handoff` must
    /// agree, or every `echo` replay silently drops the byte.
    #[test]
    fn the_handoff_preserves_a_trailing_carriage_return() {
        let log = log_of(&[
            ("postinst", Class::Log, "first line"),
            ("postinst", Class::Log, "second line\r"),
            ("postinst", Class::Log, "third line"),
        ]);
        assert_eq!(PackageLog::from_handoff(&log.to_handoff()), log);
    }

    /// Collecting consumes the files, so a build tree kept by
    /// `FEATURES=keeptemp` cannot have its messages filed twice.
    #[test]
    fn collect_consumes_the_logging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let logging = camino::Utf8Path::from_path(dir.path()).unwrap().to_owned();
        std::fs::write(logging.join("postinst"), "LOG one\n").unwrap();

        assert_eq!(
            PackageLog::collect(&logging),
            log_of(&[("postinst", Class::Log, "one")])
        );
        assert!(PackageLog::collect(&logging).is_empty());
        assert!(!logging.join("postinst").exists());
    }

    /// A directory `em` creates gets the group and setgid bit; one that already
    /// exists is left exactly as the administrator set it.
    #[test]
    fn the_elogdir_is_only_shaped_when_created() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let base = camino::Utf8Path::from_path(dir.path()).unwrap();

        let fresh = base.join("fresh");
        ensure_elogdir(&fresh).unwrap();
        let mode = std::fs::metadata(fresh.as_std_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o2770, "setgid + group rwx");

        // An existing directory keeps whatever it had.
        let existing = base.join("existing");
        std::fs::create_dir_all(existing.as_std_path()).unwrap();
        std::fs::set_permissions(
            existing.as_std_path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        ensure_elogdir(&existing).unwrap();
        assert_eq!(
            std::fs::metadata(existing.as_std_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
    }

    /// `write_elog_file` forces group-readable (0o660) regardless of umask, so
    /// `em read` can display a file a sudo'd merge filed as
    /// `root:<invoker group>` — the directory's setgid bit only makes such
    /// files deletable, not readable.
    #[test]
    fn write_elog_file_is_group_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let elogdir = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("elog");
        write_elog_file(&elogdir, "summary.log", "body\n", true);
        let mode = std::fs::metadata(elogdir.join("summary.log"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o660,
            "elog files are group-readable (0o660)"
        );
    }

    /// A stamp is whatever is in a file name, not necessarily what `save`
    /// wrote — byte-slicing it must not panic on a char boundary.
    #[test]
    fn a_non_ascii_stamp_does_not_panic() {
        let log = SavedLog {
            path: Utf8PathBuf::from("/tmp/x.log"),
            package: "cat/pf".to_string(),
            stamp: "abcé†-aaaaaa".to_string(),
        };
        assert_eq!(log.when(), "abcé†-aaaaaa");
    }

    #[test]
    fn a_message_shaped_like_a_header_stays_a_message() {
        // Only a real phase name makes a header — `ERROR: could not` is content.
        let log = PackageLog::from_text("WARN: setup\nERROR: could not find it\n");
        assert_eq!(
            log,
            log_of(&[("setup", Class::Warn, "ERROR: could not find it")])
        );
    }

    #[test]
    fn filtering_drops_classes_and_the_phases_left_empty() {
        let log = log_of(&[
            ("setup", Class::Info, "early"),
            ("postinst", Class::Log, "kept"),
            ("postinst", Class::Info, "dropped"),
        ]);
        assert_eq!(
            log.filter(ClassSet::parse("log")),
            log_of(&[("postinst", Class::Log, "kept")])
        );
        assert!(log.filter(ClassSet::parse("qa")).is_empty());
    }

    #[test]
    fn config_follows_make_globals_defaults() {
        // The shipped default: save_summary with its own class list, plus echo
        // on PORTAGE_ELOG_CLASSES.
        let config = Config::new(
            "log warn error",
            "save_summary:log,warn,error,qa echo",
            Utf8PathBuf::from("/var/log/portage"),
        );
        assert_eq!(
            config.wants(Module::SaveSummary),
            Some(ClassSet::parse("log warn error qa"))
        );
        assert_eq!(
            config.wants(Module::Echo),
            Some(ClassSet::parse("log warn error"))
        );
        assert_eq!(config.wants(Module::Save), None);

        // Modules `em` has no equivalent for are ignored, not errors, and a
        // module with no classes left has nothing to do.
        let config = Config::new("", "syslog mail echo", Utf8PathBuf::default());
        assert!(!config.is_enabled());
        // `save-summary` and `save_summary` are the same module.
        assert!(Config::new("log", "save-summary", Utf8PathBuf::default()).is_enabled());
    }

    #[test]
    fn dispatch_writes_the_configured_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let base = camino::Utf8Path::from_path(dir.path()).unwrap();
        let logdir = base.join("log");
        let work = base.join("work");
        std::fs::create_dir_all(work.as_std_path()).unwrap();
        let cpv: portage_atom::Cpv = "sys-devel/binutils-2.45".parse().unwrap();
        let log = log_of(&[
            ("postinst", Class::Log, "kept"),
            ("postinst", Class::Info, "filtered out"),
        ]);

        let config = Config::new("log", "save save_summary echo", logdir.clone());
        dispatch(&config, &cpv, &log, Echo::Handoff(&work));

        let elogdir = logdir.join("elog");
        let saved: Vec<_> = std::fs::read_dir(elogdir.as_std_path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let per_package = saved
            .iter()
            .find(|n| n.starts_with("sys-devel:binutils-2.45:"))
            .expect("per-package elog file");
        assert!(per_package.ends_with(".log"));
        assert_eq!(
            std::fs::read_to_string(elogdir.join(per_package).as_std_path()).unwrap(),
            "LOG: postinst\nkept\n"
        );

        let summary = std::fs::read_to_string(elogdir.join("summary.log").as_std_path()).unwrap();
        assert!(summary.contains("for package sys-devel/binutils-2.45:"));
        assert!(summary.contains("LOG: postinst\nkept\n"));
        // The filtered-out class never reaches any module.
        assert!(!summary.contains("filtered out"));

        // echo leaves its text for the parent instead of printing here — in
        // the unambiguous handoff shape, not the combined-log one.
        assert_eq!(
            std::fs::read_to_string(work.join(PENDING_ECHO_FILE).as_std_path()).unwrap(),
            "LOG postinst kept\n"
        );

        // With elog off nothing is written at all.
        let off = base.join("off");
        dispatch(
            &Config::new("log", "", off.clone()),
            &cpv,
            &log,
            Echo::Handoff(work.as_path()),
        );
        assert!(!off.exists());
    }

    #[test]
    fn saved_logs_read_both_layouts_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let elogdir = camino::Utf8Path::from_path(dir.path()).unwrap();
        let write = |rel: &str| {
            let path = elogdir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap().as_std_path()).unwrap();
            std::fs::write(path.as_std_path(), "LOG: postinst\nhi\n").unwrap();
        };
        write("app-misc:probe-1:20260805-061818.log");
        write("sys-devel:binutils-2.45:20260804-120000.log");
        // FEATURES=split-elog puts the category in a directory instead.
        write("dev-libs/openssl-3.6:20260806-000000.log");
        // Neither of these is a per-package file.
        write("summary.log");
        std::fs::create_dir_all(elogdir.join("empty-cat").as_std_path()).unwrap();

        let logs = saved_logs(elogdir);
        let names: Vec<_> = logs.iter().map(|l| l.package.as_str()).collect();
        assert_eq!(
            names,
            [
                "dev-libs/openssl-3.6",
                "app-misc/probe-1",
                "sys-devel/binutils-2.45"
            ]
        );
        assert_eq!(logs[1].when(), "2026-08-05 06:18:18");

        // A round trip through the saved file gets the structure back.
        let text = std::fs::read_to_string(logs[1].path.as_std_path()).unwrap();
        assert_eq!(
            PackageLog::from_text(&text),
            log_of(&[("postinst", Class::Log, "hi")])
        );
    }

    #[test]
    fn the_filename_stamp_is_portages_shape() {
        let stamp = timestamp();
        let (date, time) = stamp.split_once('-').unwrap();
        assert_eq!(date.len(), 8, "{stamp}");
        assert_eq!(time.len(), 6, "{stamp}");
        assert!(
            stamp.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "{stamp}"
        );
    }
}
