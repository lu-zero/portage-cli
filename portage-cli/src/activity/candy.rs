//! `FEATURES=candy`: live progress bars for a merge, drawn from the activity bus
//!
//! One bar for the whole plan with an ETA from the duration history, and one per running package
//! with its phase, its time against its usual build time, and the last line of its build log. The
//! text sink only reports failures, above the bars.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use portage_atom::Cpv;

use super::bus::ActivitySink;
use super::event::{ActivityEvent, ActivityMergeRoot, ActivityMode, ActivityPlanPkg};
use super::history::{DurationStore, critical_path_wall, format_seconds};

/// How often the per-package bars and log lines refresh
const TICK: Duration = Duration::from_millis(250);

/// Past builds the per-package and overall estimates take the median of
const HISTORY: usize = 15;

/// How much of a build log's end is read for its last line
const TAIL_BYTES: u64 = 4096;

struct Running {
    /// The package's plan entry, when it has one
    index: Option<usize>,
    bar: ProgressBar,
    log: Option<Utf8PathBuf>,
    estimate: Option<f64>,
    started: Instant,
}

struct Session {
    job_id: Arc<str>,
    multi: MultiProgress,
    total: ProgressBar,
    plan: Vec<ActivityPlanPkg>,
    /// Each plan entry's usual build time, when there is history for it
    estimates: Vec<Option<f64>>,
    blockers: Vec<Vec<usize>>,
    jobs: u32,
    done: HashSet<usize>,
    failed: u32,
    running: HashMap<Arc<Cpv>, Running>,
}

/// Terminal renderer for `FEATURES=candy`, attached next to the banner sink
#[derive(Default)]
pub struct CandySink {
    session: Arc<Mutex<Option<Session>>>,
    ticker: Mutex<Option<(Arc<AtomicBool>, JoinHandle<()>)>>,
}

impl CandySink {
    /// A sink with no session yet; the next merge `SessionStart` draws its bars
    pub fn new() -> Self {
        Self::default()
    }

    fn start_ticker(&self) {
        let stop = Arc::new(AtomicBool::new(false));
        let session = Arc::clone(&self.session);
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                if let Some(s) = session.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    for running in s.running.values() {
                        refresh(running);
                    }
                    s.update_total();
                }
            }
        });
        *self.ticker.lock().unwrap_or_else(|e| e.into_inner()) = Some((stop, handle));
    }

    fn stop_ticker(&self) {
        if let Some((stop, handle)) = self.ticker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
    }
}

fn total_style() -> ProgressStyle {
    ProgressStyle::with_template("[{bar:30.green/blue}] {pos}/{len} packages {msg}")
        .expect("template")
        .progress_chars("=>-")
}

fn package_style(estimated: bool) -> ProgressStyle {
    let template = if estimated {
        "{spinner:.green} [{bar:20.cyan/blue}] {elapsed:>4} {prefix}\n    {wide_msg:.dim}"
    } else {
        "{spinner:.green} {elapsed:>4} {prefix}\n    {wide_msg:.dim}"
    };
    ProgressStyle::with_template(template)
        .expect("template")
        .progress_chars("=>-")
        .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ")
}

fn prefix(cpv: &Cpv, phase: Option<&str>, estimate: Option<f64>) -> String {
    let mut out = cpv.to_string();
    if let Some(phase) = phase {
        out.push(' ');
        out.push_str(phase);
    }
    if let Some(secs) = estimate {
        out.push_str(&format!(" (usually {})", format_seconds(secs)));
    }
    out
}

fn refresh(running: &Running) {
    if running.estimate.is_some() {
        let elapsed = running.started.elapsed().as_secs();
        let len = running.bar.length().unwrap_or(1);
        running.bar.set_position(elapsed.min(len));
    } else {
        running.bar.tick();
    }
    if let Some(line) = running
        .log
        .as_deref()
        .and_then(|log| last_line(log.as_std_path()))
    {
        running.bar.set_message(line);
    }
}

/// The last line with any text in the end of `log`, without escape sequences or `\r` redraws
fn last_line(log: &std::path::Path) -> Option<String> {
    let mut file = std::fs::File::open(log).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    tail.split(|&b| b == b'\n')
        .rev()
        .filter_map(|line| {
            line.split(|&b| b == b'\r')
                .rev()
                .map(visible_text)
                .find(|t| !t.is_empty())
        })
        .next()
}

/// `bytes` as text without ANSI escape sequences and surrounding blanks
fn visible_text(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().copied().peekable();
    while let Some(b) = iter.next() {
        if b != 0x1b {
            if b >= 0x20 || b == b'\t' {
                out.push(b);
            }
            continue;
        }
        // CSI (`ESC [ ... final`) ends at a byte in 0x40..=0x7e; any other escape is two bytes.
        if iter.peek() == Some(&b'[') {
            iter.next();
            for c in iter.by_ref() {
                if (0x40..=0x7e).contains(&c) {
                    break;
                }
            }
        } else {
            iter.next();
        }
    }
    String::from_utf8_lossy(&out).trim().to_string()
}

impl Session {
    /// Count what is done and estimate the rest: running packages for their remaining time,
    /// in the build order `--jobs` allows
    fn update_total(&self) {
        let keep: Vec<usize> = (0..self.plan.len())
            .filter(|i| !self.done.contains(i))
            .collect();
        let position: HashMap<usize, usize> =
            keep.iter().enumerate().map(|(n, &i)| (i, n)).collect();
        let elapsed: HashMap<usize, f64> = self
            .running
            .values()
            .filter_map(|r| Some((r.index?, r.started.elapsed().as_secs_f64())))
            .collect();
        let mut unknown = 0;
        let durations: Vec<f64> = keep
            .iter()
            .map(|i| match self.estimates[*i] {
                Some(secs) => (secs - elapsed.get(i).copied().unwrap_or(0.0)).max(0.0),
                None => {
                    unknown += 1;
                    0.0
                }
            })
            .collect();
        let blockers: Vec<Vec<usize>> = if self.blockers.len() == self.plan.len() {
            keep.iter()
                .map(|&i| {
                    self.blockers[i]
                        .iter()
                        .filter_map(|p| position.get(p).copied())
                        .collect()
                })
                .collect()
        } else {
            keep.iter().map(|_| Vec::new()).collect()
        };
        let mut msg = String::new();
        if unknown < keep.len() {
            let wall = critical_path_wall(&durations, &blockers, self.jobs as usize);
            msg.push_str(&format!("· ETA ~{}", format_seconds(wall)));
            if unknown > 0 {
                msg.push_str(&format!(" + {unknown} without history"));
            }
        }
        if self.failed > 0 {
            msg.push_str(&format!(" · {} failed", self.failed));
        }
        self.total.set_position(self.done.len() as u64);
        self.total.set_message(msg);
    }

    /// The plan entry a package event is about: the first unfinished one with its cpv and root
    fn plan_index(&self, cpv: &Cpv, merge_root: ActivityMergeRoot) -> Option<usize> {
        (0..self.plan.len()).find(|i| {
            !self.done.contains(i)
                && *self.plan[*i].cpv == *cpv
                && self.plan[*i].merge_root == merge_root
        })
    }
}

impl ActivitySink for CandySink {
    fn on_event(&self, event: &ActivityEvent) {
        match event {
            ActivityEvent::SessionStart {
                job_id,
                mode,
                merge_root,
                host_root,
                plan_total,
                flags,
                plan,
                blockers,
                ..
            } if *mode == ActivityMode::Merge => {
                let multi = MultiProgress::new();
                let total = multi.add(ProgressBar::new(u64::from(*plan_total)));
                total.set_style(total_style());
                // A root merged into for the first time has little history; the host's own
                // builds of the same packages are the next best guess.
                let target = DurationStore::load(camino::Utf8Path::new(merge_root.as_ref()));
                let host = (host_root != merge_root)
                    .then(|| DurationStore::load(camino::Utf8Path::new(host_root.as_ref())));
                let estimates = plan
                    .iter()
                    .map(|p| {
                        target
                            .median_seconds(p.cpn, HISTORY)
                            .or_else(|| host.as_ref()?.median_seconds(p.cpn, HISTORY))
                    })
                    .collect();
                let session = Session {
                    job_id: job_id.clone(),
                    multi: multi.clone(),
                    total,
                    plan: plan.clone(),
                    estimates,
                    blockers: blockers.clone(),
                    jobs: flags.jobs.unwrap_or(1).max(1),
                    done: HashSet::new(),
                    failed: 0,
                    running: HashMap::new(),
                };
                session.update_total();
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
                crate::console::set_bars(Some(multi));
                self.start_ticker();
            }
            ActivityEvent::SessionEnd {
                job_id,
                failed,
                seconds,
                ..
            } => {
                let ended = {
                    let mut slot = self.session.lock().unwrap_or_else(|e| e.into_inner());
                    match slot.as_ref() {
                        Some(s) if s.job_id == *job_id => slot.take(),
                        _ => None,
                    }
                };
                let Some(session) = ended else {
                    return;
                };
                self.stop_ticker();
                crate::console::set_bars(None);
                for running in session.running.values() {
                    running.bar.finish_and_clear();
                }
                let mut summary = format!("in {}", format_seconds(*seconds));
                if *failed > 0 {
                    summary.push_str(&format!(" · {failed} failed"));
                }
                session.total.abandon_with_message(summary);
            }
            ActivityEvent::PkgStart {
                job_id,
                cpv,
                merge_root,
                log,
                ..
            } => {
                let mut slot = self.session.lock().unwrap_or_else(|e| e.into_inner());
                let Some(s) = slot.as_mut().filter(|s| s.job_id == *job_id) else {
                    return;
                };
                let index = s.plan_index(cpv, *merge_root);
                let estimate = index.and_then(|i| s.estimates[i]);
                let bar = s.multi.add(match estimate {
                    Some(secs) => ProgressBar::new((secs.ceil() as u64).max(1)),
                    None => ProgressBar::no_length(),
                });
                bar.set_style(package_style(estimate.is_some()));
                bar.set_prefix(prefix(cpv, None, estimate));
                s.running.insert(
                    cpv.clone(),
                    Running {
                        index,
                        bar,
                        log: log.as_deref().map(Utf8PathBuf::from),
                        estimate,
                        started: Instant::now(),
                    },
                );
            }
            ActivityEvent::PhaseEnter {
                job_id, cpv, phase, ..
            } => {
                let slot = self.session.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(running) = slot
                    .as_ref()
                    .filter(|s| s.job_id == *job_id)
                    .and_then(|s| s.running.get(cpv))
                {
                    running
                        .bar
                        .set_prefix(prefix(cpv, Some(phase), running.estimate));
                }
            }
            ActivityEvent::PkgEnd {
                job_id,
                cpv,
                merge_root,
                ok,
                ..
            } => {
                let mut slot = self.session.lock().unwrap_or_else(|e| e.into_inner());
                let Some(s) = slot.as_mut().filter(|s| s.job_id == *job_id) else {
                    return;
                };
                let running = s.running.remove(cpv);
                if let Some(running) = &running {
                    running.bar.finish_and_clear();
                    s.multi.remove(&running.bar);
                }
                let index = running
                    .and_then(|r| r.index)
                    .or_else(|| s.plan_index(cpv, *merge_root));
                if let Some(index) = index {
                    s.done.insert(index);
                }
                if !*ok {
                    s.failed += 1;
                }
                s.update_total();
            }
            _ => {}
        }
    }
}

impl Drop for CandySink {
    fn drop(&mut self) {
        self.stop_ticker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{ACTIVITY_EVENT_VERSION, ActivityMergeRoot, PkgKind, SessionFlags};

    fn plan_pkg(cpv: &str, merge_root: ActivityMergeRoot) -> ActivityPlanPkg {
        let cpv: Cpv = cpv.parse().expect("test cpv parses");
        ActivityPlanPkg {
            cpn: cpv.cpn,
            cpv: Arc::new(cpv),
            merge_root,
        }
    }

    fn pkg_end(cpv: &str, merge_root: ActivityMergeRoot, ok: bool) -> ActivityEvent {
        let cpv: Cpv = cpv.parse().expect("test cpv parses");
        ActivityEvent::PkgEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            cpn: cpv.cpn,
            cpv: Arc::new(cpv),
            merge_root,
            kind: PkgKind::Source,
            ok,
            at: 2.0,
            seconds: 1.0,
            phases: Vec::new(),
            error: None,
            failed_phase: None,
            log: None,
        }
    }

    // The same cpv built for the host and for the target is two plan entries: finishing one
    // leaves the other to do, and a failure is counted as done and as failed.
    #[test]
    fn a_finished_package_is_its_own_plan_entry() {
        let sink = CandySink::new();
        let root = tempfile::tempdir().expect("tempdir");
        sink.on_event(&ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            pid: 1,
            started_at: 0.0,
            argv: Vec::new(),
            merge_root: root.path().to_str().expect("utf-8 tempdir").into(),
            host_root: "/".into(),
            base_root: "".into(),
            mode: ActivityMode::Merge,
            plan_total: 2,
            flags: SessionFlags::default(),
            plan: vec![
                plan_pkg("dev-libs/a-1", ActivityMergeRoot::Host),
                plan_pkg("dev-libs/a-1", ActivityMergeRoot::Target),
            ],
            blockers: vec![vec![], vec![0]],
        });
        sink.on_event(&pkg_end("dev-libs/a-1", ActivityMergeRoot::Target, false));
        {
            let slot = sink.session.lock().expect("session lock");
            let s = slot.as_ref().expect("session started");
            assert_eq!(s.done, HashSet::from([1]));
            assert_eq!(s.failed, 1);
            assert_eq!(s.total.position(), 1);
        }
        sink.on_event(&ActivityEvent::SessionEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id: "j".into(),
            parent_job_id: None,
            at: 3.0,
            ok: false,
            completed: 1,
            failed: 1,
            seconds: 3.0,
        });
        assert!(sink.session.lock().expect("session lock").is_none());
    }

    fn tail_of(content: &[u8]) -> Option<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("build.log");
        std::fs::write(&path, content).expect("write log");
        last_line(&path)
    }

    // A compiler's colours and a progress counter's `\r` redraws leave only the text last shown.
    #[test]
    fn last_line_shows_the_text_a_terminal_would() {
        let log = b"checking for gcc... yes\n\x1b[01;32mCC\x1b[0m foo.o\n[ 10%]\r[ 55%]\r\n\n";
        assert_eq!(tail_of(log).as_deref(), Some("[ 55%]"));
        assert_eq!(tail_of(b"one\ntwo\n   \n").as_deref(), Some("two"));
        assert_eq!(tail_of(b""), None);
    }

    // Only the end of a long log is read.
    #[test]
    fn last_line_reads_only_the_tail() {
        let mut log = vec![b'x'; 10 * TAIL_BYTES as usize];
        log.extend_from_slice(b"\nlast words\n");
        assert_eq!(tail_of(&log).as_deref(), Some("last words"));
    }
}
