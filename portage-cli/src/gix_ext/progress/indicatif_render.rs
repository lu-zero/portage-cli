//! Poll a prodash progress tree into an indicatif multi-progress display
//!
//! gix mutates prodash `Item`s (including via `counter()` atomics). We never
//! implement NestedProgress ourselves — we only snapshot the tree.
//!
//! **0 B for a while is normal:** negotiation and waiting for the first pack
//! bytes leave counters at zero; we label that as waiting + elapsed so it does
//! not look hung.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use humansize::{BINARY, format_size};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use prodash::progress::Key;

pub struct Guard {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn spawn(root: Arc<prodash::tree::Root>) -> Guard {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let join = std::thread::Builder::new()
        .name("em-sync-progress".into())
        .spawn(move || run(root, stop_t))
        .expect("spawn progress UI thread");
    Guard {
        stop,
        join: Some(join),
    }
}

fn run(root: Arc<prodash::tree::Root>, stop: Arc<AtomicBool>) {
    let multi = MultiProgress::new();
    let mut bars: HashMap<Key, ProgressBar> = HashMap::new();
    // When we first saw this key still at step 0 (for "waiting" elapsed).
    let mut zero_since: HashMap<Key, Instant> = HashMap::new();
    let mut snapshot: Vec<(Key, prodash::progress::Task)> = Vec::new();
    let session_start = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        snapshot.clear();
        root.sorted_snapshot(&mut snapshot);

        let mut seen = HashMap::new();
        for (key, task) in &snapshot {
            seen.insert(*key, ());
            let Some(value) = task.progress.as_ref() else {
                // Organizational node (name only) — spinner + elapsed.
                let bar = bars.entry(*key).or_insert_with(|| {
                    let b = multi.add(ProgressBar::new_spinner());
                    b.set_style(spinner_style());
                    b.enable_steady_tick(Duration::from_millis(80));
                    b
                });
                bar.set_message(format!(
                    "{}  ({})",
                    task.name,
                    format_elapsed(session_start.elapsed())
                ));
                continue;
            };

            let step = value.step.load(Ordering::Relaxed);
            let bar = bars.entry(*key).or_insert_with(|| {
                let b = multi.add(ProgressBar::new_spinner());
                b.set_style(spinner_style());
                b.enable_steady_tick(Duration::from_millis(80));
                b
            });

            match value.done_at {
                Some(max) if max > 0 => {
                    bar.set_style(bar_style());
                    bar.set_length(max as u64);
                }
                _ => {
                    bar.set_style(spinner_style());
                }
            }
            bar.set_position(step as u64);

            let label = if step == 0 {
                let since = zero_since.entry(*key).or_insert_with(Instant::now);
                format!(
                    "{}  waiting… ({})",
                    task.name,
                    format_elapsed(since.elapsed())
                )
            } else {
                zero_since.remove(key);
                match value.done_at {
                    Some(max) if max > 0 => format!(
                        "{}  {} / {}",
                        task.name,
                        format_size(step as u64, BINARY),
                        format_size(max as u64, BINARY)
                    ),
                    _ => format!("{}  {}", task.name, format_size(step as u64, BINARY)),
                }
            };
            bar.set_message(label);
        }

        // Drop bars for tasks that vanished from the tree.
        bars.retain(|key, bar| {
            if seen.contains_key(key) {
                true
            } else {
                zero_since.remove(key);
                bar.finish_and_clear();
                false
            }
        });

        std::thread::sleep(Duration::from_millis(80));
    }

    for (_, bar) in bars.drain() {
        bar.finish_and_clear();
    }
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.green} {msg}")
        .expect("template")
        .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ")
}

fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.green} {msg} [{bar:24.cyan/blue}]")
        .expect("template")
        .progress_chars("=>-")
        .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ")
}
