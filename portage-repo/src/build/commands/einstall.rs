use std::io::Write;

use brush_core::builtins;
use clap::Parser;

use super::die::DieFlag;

/// `einstall [args...]` — pre-EAPI-6 install helper (banned in EAPI 6+)
///
/// Runs `${MAKE:-make}` with GNU-style install path overrides pointing into
/// `${ED}`, for legacy build systems that ignore `DESTDIR`. Mirrors portage's
/// `phase-helpers.sh` `einstall`. EAPI 7+ ebuilds use `emake DESTDIR=… install`
/// instead; this exists only for completeness with old ebuilds.
#[derive(Parser)]
pub(crate) struct EinstallCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

/// Raise the shared `die` flag and echo the portage-style message, returning
/// exit 1 (the "die: " prefix is matched by tests and `inherit` error paths).
fn die<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
    msg: &str,
) -> brush_core::ExecutionResult {
    if let Ok(flag) = context.shared::<DieFlag>() {
        flag.raise(msg);
    }
    let _ = writeln!(context.params.stderr(context.shell), "die: {msg}");
    brush_core::ExecutionResult::new(1)
}

impl builtins::Command for EinstallCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = &*context.shell;
        let getv = |name: &str| {
            shell
                .env_str(name)
                .map(|s| s.into_owned())
                .unwrap_or_default()
        };

        // PMS: einstall is available only for EAPI 0-5 (banned in 6+).
        let eapi = getv("EAPI");
        if !matches!(eapi.as_str(), "0" | "1" | "2" | "3" | "4" | "5") {
            return Ok(die(
                &context,
                &format!("'einstall' has been banned for EAPI '{eapi}'"),
            ));
        }

        let cwd = shell.working_dir().to_path_buf();
        if !["Makefile", "GNUmakefile", "makefile"]
            .iter()
            .any(|m| cwd.join(m).is_file())
        {
            return Ok(die(&context, "no Makefile found"));
        }

        // ED == D for EAPI without prefix variables (0-2); for 3-5 ED is set
        // with EPREFIX already. Fall back to D when ED is unset.
        let ed_raw = {
            let ed = getv("ED");
            if ed.is_empty() { getv("D") } else { ed }
        };
        let ed = ed_raw.trim_end_matches('/').to_string();

        let make = {
            let m = getv("MAKE");
            if m.is_empty() { "make".to_string() } else { m }
        };
        let split =
            |v: String| -> Vec<String> { v.split_whitespace().map(str::to_owned).collect() };
        let makeopts = split(getv("MAKEOPTS"));
        let extra_emake = split(getv("EXTRA_EMAKE"));

        // LOCAL_EXTRA_EINSTALL: a libdir override (when ABI/LIBDIR_$ABI and
        // CONF_PREFIX are set) followed by EXTRA_EINSTALL.
        let mut local_extra: Vec<String> = Vec::new();
        let abi = getv("ABI");
        let libdir = if abi.is_empty() {
            String::new()
        } else {
            getv(&format!("LIBDIR_{abi}"))
        };
        if !libdir.is_empty() && shell.env_is_set("CONF_PREFIX") {
            let d = getv("D");
            let mut destlibdir = format!(
                "{}/{}/{libdir}",
                d.trim_end_matches('/'),
                getv("CONF_PREFIX")
            );
            while destlibdir.contains("//") {
                destlibdir = destlibdir.replace("//", "/");
            }
            local_extra.push(format!("libdir={destlibdir}"));
        }
        local_extra.extend(split(getv("EXTRA_EINSTALL")));

        let path_args = [
            format!("prefix={ed}/usr"),
            format!("datadir={ed}/usr/share"),
            format!("infodir={ed}/usr/share/info"),
            format!("localstatedir={ed}/var/lib"),
            format!("mandir={ed}/usr/share/man"),
            format!("sysconfdir={ed}/etc"),
        ];
        let user_args = self.args.clone();
        let (stdout, stderr) = super::context_stdio(&context);

        let exit = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&make)
                .current_dir(&cwd)
                .args(&path_args)
                .args(&local_extra)
                .args(&makeopts)
                .arg("-j1")
                .args(&user_args)
                .args(&extra_emake)
                .arg("install")
                .stdout(stdout)
                .stderr(stderr)
                .status()
                .map(|s| s.code().unwrap_or(1) as u8)
                .unwrap_or(127)
        })
        .await
        .unwrap_or(127);

        if exit != 0 {
            return Ok(die(&context, "einstall failed"));
        }
        Ok(brush_core::ExecutionResult::new(0))
    }
}
