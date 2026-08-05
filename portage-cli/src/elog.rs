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
//! The file the worker leaves behind is in portage's own combined-log format,
//! not a private one: it is the same text `save`/`save_summary` write, so
//! nothing here needs a serialization of its own.

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

    /// Read `${T}/logging/`, portage's `collect_ebuild_messages`.
    ///
    /// A missing directory is simply an empty log — the phase may never have
    /// run, or nothing may have called an `e*` function.
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
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    /// Inverse of [`to_text`](Self::to_text) — how the `echo` handoff and
    /// `em read` get structure back out of a combined log.
    ///
    /// A line only counts as a header if its phase is a real one, so an
    /// ordinary message that happens to read like `ERROR: something` stays a
    /// message.
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
        let systems = systems
            .split_whitespace()
            .filter_map(|spec| {
                let (name, classes) = match spec.split_once(':') {
                    Some((name, list)) => (name, ClassSet::parse(list)),
                    None => (spec, default_classes),
                };
                let module = Module::parse(name)?;
                (!classes.is_empty()).then_some((module, classes))
            })
            .collect();
        Self { systems, logdir }
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

/// Run the file-writing modules for one merged package and, if `echo` is
/// configured, leave its text in `<work_dir>/elog` for the parent.
///
/// Never fails the merge: a package that built and installed correctly must not
/// be reported as broken because its log could not be filed. Problems are
/// warned about instead.
pub fn dispatch(config: &Config, cpv: &portage_atom::Cpv, log: &PackageLog, work_dir: &Utf8Path) {
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
        if !filtered.is_empty() {
            let path = work_dir.join(PENDING_ECHO_FILE);
            if let Err(e) = std::fs::write(path.as_std_path(), filtered.to_text()) {
                crate::style::warn_line!("elog: cannot write {path}: {e}");
            }
        }
    }
}

/// `<pn>-<version>`, the `PF` half of an elog file name.
fn pf(cpv: &portage_atom::Cpv) -> String {
    format!("{}-{}", cpv.cpn.package, cpv.version)
}

fn write_elog_file(elogdir: &Utf8Path, name: &str, body: &str, append: bool) {
    if let Err(e) = std::fs::create_dir_all(elogdir.as_std_path()) {
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
static PENDING: OnceLock<Mutex<Vec<(String, PackageLog)>>> = OnceLock::new();

fn pending() -> &'static Mutex<Vec<(String, PackageLog)>> {
    PENDING.get_or_init(Mutex::default)
}

/// Take what the merge chain left in `<work_dir>/elog` and hold it for
/// [`finalize_echo`]. Called by the parent once a package has merged, whether
/// or not the chain ran in a `__worker` child.
pub fn take_pending(cpv: &portage_atom::Cpv, work_dir: &Utf8Path) {
    let path = work_dir.join(PENDING_ECHO_FILE);
    let Ok(text) = std::fs::read_to_string(path.as_std_path()) else {
        return;
    };
    let _ = std::fs::remove_file(path.as_std_path());
    let log = PackageLog::from_text(&text);
    if !log.is_empty()
        && let Ok(mut queue) = pending().lock()
    {
        queue.push((cpv.to_string(), log));
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
    for (cpv, log) in &items {
        let info = crate::style::PORTAGE_COLORS.info;
        let pkg = crate::style::C_PKG;
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            " {info}*{info:#} Messages for package {pkg}{cpv}{pkg:#}:"
        );
        let _ = writeln!(out);
        log.print_to(&mut out);
    }
    let _ = out.flush();
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
            "INFO: setup\nearly\nLOG: postinst\none\ntwo\nWARN: postinst\ncareful\n\n"
        );
        assert_eq!(PackageLog::from_text(&text), log);

        // An empty log has no trailing blank line to strip.
        assert_eq!(PackageLog::default().to_text(), "");
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
        dispatch(&config, &cpv, &log, &work);

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
            "LOG: postinst\nkept\n\n"
        );

        let summary = std::fs::read_to_string(elogdir.join("summary.log").as_std_path()).unwrap();
        assert!(summary.contains("for package sys-devel/binutils-2.45:"));
        assert!(summary.contains("LOG: postinst\nkept\n"));
        // The filtered-out class never reaches any module.
        assert!(!summary.contains("filtered out"));

        // echo leaves its text for the parent instead of printing here.
        assert_eq!(
            std::fs::read_to_string(work.join(PENDING_ECHO_FILE).as_std_path()).unwrap(),
            "LOG: postinst\nkept\n\n"
        );

        // With elog off nothing is written at all.
        let off = base.join("off");
        dispatch(
            &Config::new("log", "", off.clone()),
            &cpv,
            &log,
            work.as_path(),
        );
        assert!(!off.exists());
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
