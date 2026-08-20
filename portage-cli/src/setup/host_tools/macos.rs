//! macOS: the system's own `sed`/`grep` are BSD, and the GNU builds install
//! under `g`-prefixed names (or in a directory deliberately kept off `PATH`)
//! so they don't clobber them. Look where the two package managers put them.

use camino::{Utf8Path, Utf8PathBuf};

/// Homebrew's standard prefixes: Apple Silicon, then Intel
const BREW_PREFIXES: &[&str] = &["/opt/homebrew", "/usr/local"];

const MACPORTS_BIN: &str = "/opt/local/bin";

pub(super) fn candidates(brew: Option<&str>, name: &str) -> Vec<Utf8PathBuf> {
    let mut out: Vec<Utf8PathBuf> = brew
        .iter()
        .flat_map(|formula| brew_prefixes(formula))
        // `libexec/gnubin` is where the plain, unprefixed names live.
        .flat_map(|opt| [opt.join("libexec/gnubin"), opt.join("bin")])
        .map(|dir| dir.join(name))
        .collect();
    out.push(Utf8Path::new(MACPORTS_BIN).join(name));
    out
}

/// `<prefix>/opt/<formula>` for each Homebrew install
///
/// Asked of `brew` itself when it is on `PATH`, since a user may have installed it
/// elsewhere.
fn brew_prefixes(formula: &str) -> Vec<Utf8PathBuf> {
    if let Ok(out) = std::process::Command::new("brew")
        .args(["--prefix", formula])
        .output()
        && out.status.success()
    {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !path.is_empty() {
            return vec![Utf8PathBuf::from(path)];
        }
    }
    BREW_PREFIXES
        .iter()
        .map(|p| Utf8Path::new(p).join("opt").join(formula))
        .collect()
}

pub(super) fn install_hint(brew: Option<&str>, name: &str) -> String {
    match brew {
        Some(formula) => format!("Install it with `brew install {formula}`, then re-run em setup."),
        None => format!(
            "Install the Xcode command line tools (`xcode-select --install`), \
             or {name} via `brew install`."
        ),
    }
}
