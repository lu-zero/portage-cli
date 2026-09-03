//! Test-only usage-rs 6 CLI twin. Not wired into `em`.
//!
//! Variant 2 (`twin`) is the grammar spike: `default_subcommand = "emerge"`,
//! Topology flattened onto the root with inner `global`, and same-type flatten of
//! `MergeFlags` / `RootArg` onto parent and child. Variant 1 keeps `--prefix` only
//! on applets. Variant 3 documents `args_conflicts_with_subcommands`.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use usage::test::{self as harness, Outcome, Page};
use usage::{Args, Cli, Subcommands, ValidationError, ValueEnum};

fn os<'a>(argv: &'a [&'a str]) -> Vec<&'a OsStr> {
    argv.iter().copied().map(OsStr::new).collect()
}

fn err_name(e: &usage::Error<'_, '_>) -> String {
    match e {
        usage::Error::UnknownFlag { token } => {
            format!("UnknownFlag {}", String::from_utf8_lossy(token))
        }
        usage::Error::SubcommandConflict { .. } => "SubcommandConflict".into(),
        usage::Error::MissingArgsHelp { .. } => "MissingArgsHelp".into(),
        usage::Error::Help { .. } => "Help".into(),
        usage::Error::Version { .. } => "Version".into(),
        usage::Error::InvalidValue(v) => {
            format!("InvalidValue {} {}", v.name, v.reason)
        }
        usage::Error::MissingRequired { name } => format!("MissingRequired {name}"),
        other => format!("{other:?}"),
    }
}

/// Shared mixins for the real twin. Topology inner fields are `global`; RootArg's
/// inner field is not. Flatten-site `global` is locked by `flatten_site_global_is_rejected`.
mod mixins {
    use super::*;

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct Topology {
        #[usage(long, global, value_name = "DIR")]
        pub prefix: Option<String>,
        #[usage(long, global, default_missing = "", value_name = "DIR")]
        pub local: Option<String>,
        #[usage(long, global, value_name = "PATH")]
        pub config_root: Option<String>,
        #[usage(long, global, value_name = "PATH")]
        pub vdb: Option<String>,
        #[usage(long, short = 'T', global, value_name = "TUPLE")]
        pub target: Option<String>,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct RootArg {
        #[usage(long, value_name = "PATH")]
        pub root: Option<String>,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct MergeFlags {
        #[usage(short = 'a', long)]
        pub ask: bool,
        #[usage(short = 'u', long)]
        pub update: bool,
        #[usage(short = '1', long = "oneshot")]
        pub oneshot: bool,
        #[usage(short = 'e', long)]
        pub emptytree: bool,
        #[usage(short = 't', long)]
        pub tree: bool,
        #[usage(long)]
        pub json: bool,
        #[usage(short = 'o', long)]
        pub onlydeps: bool,
        #[usage(short = 'n', long)]
        pub noreplace: bool,
        #[usage(short = 'j', long, value_name = "N")]
        pub jobs: Option<u32>,
        #[usage(short = 'X', long, value_name = "ATOM")]
        pub exclude: Vec<String>,
        #[usage(long)]
        pub with_bdeps: bool,
        #[usage(long = "root-deps")]
        pub root_deps: bool,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct DepgraphFlags {
        #[usage(short = 'D', long)]
        pub deep: bool,
        #[usage(short = 'N', long)]
        pub newuse: bool,
        #[usage(short = 'U', long = "changed-use")]
        pub changed_use: bool,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct EmergeModeArgs {
        #[usage(short = 's', long)]
        pub search: bool,
        #[usage(short = 'C', long)]
        pub unmerge: bool,
        #[usage(short = 'c', long)]
        pub depclean: bool,
    }

    #[derive(ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum Privilege {
        #[default]
        Auto,
        Sudo,
        None,
    }
}

/// `--prefix` only on applets. Prefix before a named applet is UnknownFlag.
mod v1_prefix_on_applets {
    use super::*;

    #[derive(Args, Debug, Clone, PartialEq)]
    struct Emerge {
        #[usage(long)]
        prefix: Option<String>,
        atoms: Vec<String>,
    }

    #[derive(Args, Debug, Clone, PartialEq)]
    struct Toolchain {
        #[usage(long)]
        prefix: Option<String>,
        #[usage(long)]
        setup: bool,
    }

    #[derive(Subcommands, Debug, Clone, PartialEq)]
    enum Command {
        Emerge(Emerge),
        Toolchain(Toolchain),
    }

    #[derive(Cli, Debug, Clone, PartialEq)]
    #[usage(bin = "em", unknown_flags = "error", default_subcommand = "emerge")]
    pub struct App {
        #[usage(short = 'p', long, global)]
        pub pretend: bool,
        #[usage(subcommand)]
        command: Option<Command>,
    }

    pub fn parse(argv: &[&str]) -> Result<App, String> {
        App::try_parse_from(&os(argv)).map_err(|e| err_name(&e))
    }

    pub fn show(argv: &[&str]) -> String {
        match parse(argv) {
            Ok(app) => format!("{app:?}"),
            Err(e) => e,
        }
    }
}

/// Topology on the root (inner `global`), same-type MergeFlags/RootArg flatten.
mod twin {
    use super::mixins::*;
    use super::*;

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct Emerge {
        #[usage(flatten)]
        pub root_arg: RootArg,
        #[usage(flatten)]
        pub merge_flags: MergeFlags,
        #[usage(flatten)]
        pub depgraph_flags: DepgraphFlags,
        #[usage(flatten)]
        pub mode: EmergeModeArgs,
        #[usage(long, value_enum, default = "auto")]
        pub privilege: Privilege,
        pub atoms: Vec<String>,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct Toolchain {
        #[usage(long)]
        pub setup: bool,
        #[usage(flatten)]
        pub root_arg: RootArg,
        #[usage(flatten)]
        pub merge_flags: MergeFlags,
        #[usage(flatten)]
        pub depgraph_flags: DepgraphFlags,
        #[usage(long, value_enum, default = "auto")]
        pub privilege: Privilege,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    struct Crossdev {
        #[usage(long)]
        setup: bool,
        #[usage(flatten)]
        merge_flags: MergeFlags,
        #[usage(flatten)]
        depgraph_flags: DepgraphFlags,
        #[usage(long, value_enum, default = "auto")]
        privilege: Privilege,
    }

    #[derive(Args, Debug, Clone, PartialEq)]
    pub struct Query {
        #[usage(flatten)]
        pub root_arg: RootArg,
        #[usage(subcommand)]
        command: QueryCommand,
    }

    #[derive(Subcommands, Debug, Clone, PartialEq)]
    enum QueryCommand {
        Depgraph(Depgraph),
        Belongs(Belongs),
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct Depgraph {
        pub atom: Vec<String>,
        #[usage(flatten)]
        pub depgraph_flags: DepgraphFlags,
        #[usage(short = 'e', long)]
        emptytree: bool,
        #[usage(short = 'o', long)]
        onlydeps: bool,
        #[usage(long)]
        with_bdeps: bool,
        #[usage(long = "root-deps")]
        root_deps: bool,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    struct Belongs {
        file: Vec<String>,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct Use {
        #[usage(short = 'a', long = "add", value_name = "FLAG")]
        pub add: Vec<String>,
        #[usage(short = 's', long = "subtract", value_name = "FLAG")]
        subtract: Vec<String>,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    pub struct Search {
        #[usage(short = 'a', long)]
        pub all: bool,
        pattern: Option<String>,
    }

    #[derive(Args, Debug, Clone, PartialEq)]
    struct Active {
        #[usage(subcommand)]
        command: Option<ActiveCommand>,
    }

    #[derive(Subcommands, Debug, Clone, PartialEq)]
    enum ActiveCommand {
        Set(ActiveSet),
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    struct ActiveSet {
        reference: Option<String>,
    }

    #[derive(Args, Debug, Clone, PartialEq)]
    pub struct Worker {
        #[usage(long)]
        pub root: String,
        #[usage(long)]
        pub worker_config_root: Option<String>,
    }

    #[derive(Args, Debug, Clone, PartialEq)]
    pub struct Helper {
        pub name: String,
        #[usage(double_dash = "automatic")]
        pub args: Vec<String>,
    }

    #[derive(Subcommands, Debug, Clone, PartialEq)]
    #[allow(clippy::large_enum_variant)]
    enum Applet {
        Emerge(Emerge),
        Toolchain(Toolchain),
        Crossdev(Crossdev),
        Query(Query),
        Use(Use),
        Search(Search),
        Active(Active),
        #[usage(name = "__worker", hide)]
        Worker(Worker),
        #[usage(name = "__helper", hide)]
        Helper(Helper),
    }

    #[derive(Debug)]
    pub struct Validated(pub Twin);

    impl TryFrom<Twin> for Validated {
        type Error = ValidationError;

        fn try_from(cli: Twin) -> Result<Self, Self::Error> {
            validate(&cli)?;
            Ok(Validated(cli))
        }
    }

    fn is_merge_consumer(applet: &Option<Applet>) -> bool {
        matches!(
            applet,
            None | Some(Applet::Emerge(_) | Applet::Crossdev(_) | Applet::Toolchain(_))
        )
    }

    fn validate(cli: &Twin) -> Result<(), ValidationError> {
        if cli.root_arg.root.is_some()
            && matches!(&cli.applet, Some(Applet::Crossdev(_) | Applet::Active(_)))
        {
            return Err(ValidationError::field("--root").reason("not valid with this applet"));
        }
        if !is_merge_consumer(&cli.applet) {
            if cli.merge_flags.ask {
                return Err(ValidationError::field("--ask").reason("not valid with this applet"));
            }
            if cli.merge_flags != MergeFlags::default()
                || cli.depgraph_flags != DepgraphFlags::default()
                || cli.mode != EmergeModeArgs::default()
                || cli.privilege != Privilege::Auto
            {
                return Err(
                    ValidationError::field("emerge-mixin").reason("not valid with this applet")
                );
            }
        } else if !matches!(&cli.applet, None | Some(Applet::Emerge(_)))
            && cli.mode != EmergeModeArgs::default()
        {
            return Err(ValidationError::field("emerge-mode").reason("not valid with this applet"));
        }
        Ok(())
    }

    #[derive(Cli, Debug, Clone, PartialEq)]
    #[usage(
        bin = "em",
        version,
        about = "Gentoo Portage package manager workalike",
        arg_required_else_help,
        unknown_flags = "error",
        default_subcommand = "emerge",
        try_into = Validated
    )]
    pub struct Twin {
        #[usage(short = 'p', long, global)]
        pub pretend: bool,
        #[usage(long)]
        pub info: bool,
        #[usage(short = 'v', long, count, global)]
        pub verbose: u8,
        #[usage(short = 'q', long, global)]
        pub quiet: bool,
        #[usage(flatten)]
        pub topology: Topology,
        #[usage(flatten)]
        pub root_arg: RootArg,
        #[usage(flatten)]
        pub merge_flags: MergeFlags,
        #[usage(flatten)]
        pub depgraph_flags: DepgraphFlags,
        #[usage(flatten)]
        pub mode: EmergeModeArgs,
        #[usage(long, value_enum, default = "auto")]
        pub privilege: Privilege,
        #[usage(subcommand)]
        applet: Option<Applet>,
    }

    pub fn parse(argv: &[&str]) -> Twin {
        Twin::try_parse_from(&os(argv))
            .unwrap_or_else(|e| panic!("expected parse of {argv:?}, got {}", err_name(&e)))
    }

    pub fn parse_err(argv: &[&str]) -> String {
        match Twin::try_parse_from(&os(argv)) {
            Ok(cli) => panic!("expected error for {argv:?}, got {cli:?}"),
            Err(e) => err_name(&e),
        }
    }

    pub fn parse_into(argv: &[&str]) -> Result<Validated, String> {
        Twin::try_parse_into_from(&os(argv)).map_err(|e| err_name(&e))
    }

    impl Twin {
        pub fn emerge(&self) -> &Emerge {
            match &self.applet {
                Some(Applet::Emerge(a)) => a,
                other => panic!("expected emerge, got {other:?}"),
            }
        }

        pub fn toolchain(&self) -> &Toolchain {
            match &self.applet {
                Some(Applet::Toolchain(a)) => a,
                other => panic!("expected toolchain, got {other:?}"),
            }
        }

        pub fn query(&self) -> &Query {
            match &self.applet {
                Some(Applet::Query(a)) => a,
                other => panic!("expected query, got {other:?}"),
            }
        }

        pub fn r#use(&self) -> &Use {
            match &self.applet {
                Some(Applet::Use(a)) => a,
                other => panic!("expected use, got {other:?}"),
            }
        }

        pub fn search(&self) -> &Search {
            match &self.applet {
                Some(Applet::Search(a)) => a,
                other => panic!("expected search, got {other:?}"),
            }
        }

        pub fn worker(&self) -> &Worker {
            match &self.applet {
                Some(Applet::Worker(a)) => a,
                other => panic!("expected worker, got {other:?}"),
            }
        }

        pub fn helper(&self) -> &Helper {
            match &self.applet {
                Some(Applet::Helper(a)) => a,
                other => panic!("expected helper, got {other:?}"),
            }
        }

        pub fn is_crossdev(&self) -> bool {
            matches!(self.applet, Some(Applet::Crossdev(_)))
        }

        pub fn applet_is_none(&self) -> bool {
            self.applet.is_none()
        }

        pub fn query_depgraph(&self) -> &Depgraph {
            match &self.query().command {
                QueryCommand::Depgraph(d) => d,
                other => panic!("expected depgraph, got {other:?}"),
            }
        }

        pub fn active_command_is_none(&self) -> bool {
            matches!(&self.applet, Some(Applet::Active(a)) if a.command.is_none())
        }

        pub fn active_is_set(&self) -> bool {
            matches!(
                &self.applet,
                Some(Applet::Active(a)) if matches!(a.command, Some(ActiveCommand::Set(_)))
            )
        }
    }
}

/// `args_conflicts_with_subcommands` rejects `em -p toolchain` as well as prefix-before-applet.
mod v3_args_conflicts {
    use super::*;

    #[derive(Args, Debug, Clone, PartialEq)]
    struct Emerge {
        atoms: Vec<String>,
    }

    #[derive(Args, Debug, Clone, PartialEq)]
    struct Toolchain {
        #[usage(long)]
        prefix: Option<String>,
        #[usage(long)]
        setup: bool,
    }

    #[derive(Subcommands, Debug, Clone, PartialEq)]
    enum Command {
        Emerge(Emerge),
        Toolchain(Toolchain),
    }

    #[derive(Cli, Debug, Clone, PartialEq)]
    #[usage(
        bin = "em",
        unknown_flags = "error",
        default_subcommand = "emerge",
        args_conflicts_with_subcommands
    )]
    pub struct App {
        #[usage(short = 'p', long, global)]
        pub pretend: bool,
        #[usage(long)]
        prefix: Option<String>,
        #[usage(subcommand)]
        command: Option<Command>,
    }

    pub fn parse_err(argv: &[&str]) -> String {
        match App::try_parse_from(&os(argv)) {
            Ok(app) => panic!("expected error for {argv:?}, got {app:?}"),
            Err(e) => err_name(&e),
        }
    }

    pub fn parse_ok(argv: &[&str]) -> App {
        App::try_parse_from(&os(argv))
            .unwrap_or_else(|e| panic!("expected parse of {argv:?}, got {}", err_name(&e)))
    }
}

/// Fallback when nested `--root` must cascade: do not dual-mount `RootArg`.
mod root_cascade_fallback {
    use super::*;

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    struct RootArg {
        #[usage(long, global, value_name = "PATH")]
        root: Option<String>,
    }

    #[derive(Args, Debug, Clone, PartialEq)]
    struct Query {
        #[usage(flatten)]
        root_arg: RootArg,
        #[usage(subcommand)]
        command: QueryCommand,
    }

    #[derive(Subcommands, Debug, Clone, PartialEq)]
    enum QueryCommand {
        Depgraph(Depgraph),
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    struct Depgraph {
        atom: Vec<String>,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    struct Emerge {
        #[usage(flatten)]
        root_arg: RootArg,
        atoms: Vec<String>,
    }

    #[derive(Args, Debug, Default, Clone, PartialEq)]
    struct Crossdev {
        #[usage(long)]
        setup: bool,
    }

    #[derive(Subcommands, Debug, Clone, PartialEq)]
    enum Applet {
        Emerge(Emerge),
        Query(Query),
        Crossdev(Crossdev),
    }

    #[derive(Cli, Debug, Clone, PartialEq)]
    #[usage(bin = "em", unknown_flags = "error", default_subcommand = "emerge")]
    pub struct App {
        #[usage(long, value_name = "PATH")]
        pub root: Option<String>,
        #[usage(subcommand)]
        applet: Option<Applet>,
    }

    pub fn parse(argv: &[&str]) -> App {
        App::try_parse_from(&os(argv))
            .unwrap_or_else(|e| panic!("expected parse of {argv:?}, got {}", err_name(&e)))
    }

    pub fn parse_err(argv: &[&str]) -> String {
        match App::try_parse_from(&os(argv)) {
            Ok(app) => panic!("expected error for {argv:?}, got {app:?}"),
            Err(e) => err_name(&e),
        }
    }

    impl App {
        pub fn query_root(&self) -> Option<&str> {
            match &self.applet {
                Some(Applet::Query(q)) => q.root_arg.root.as_deref(),
                _ => None,
            }
        }

        pub fn is_crossdev(&self) -> bool {
            matches!(self.applet, Some(Applet::Crossdev(_)))
        }
    }
}

// --- variant 1 ---

#[test]
fn v1_default_subcommand_routes_a_bare_atom_to_emerge() {
    let shown = v1_prefix_on_applets::show(&["em", "firefox"]);
    assert!(shown.contains("Emerge"), "{shown}");
    assert!(shown.contains("firefox"), "{shown}");
}

#[test]
fn v1_prefix_before_applet_is_unknown() {
    assert_eq!(
        v1_prefix_on_applets::show(&["em", "--prefix", "P", "firefox"]),
        "UnknownFlag --prefix"
    );
    assert_eq!(
        v1_prefix_on_applets::show(&["em", "--prefix", "P", "toolchain", "--setup"]),
        "UnknownFlag --prefix"
    );
}

#[test]
fn v1_prefix_on_the_applet_parses() {
    let shown = v1_prefix_on_applets::show(&["em", "toolchain", "--prefix", "P", "--setup"]);
    assert!(shown.contains("Toolchain"), "{shown}");
    assert!(shown.contains("Some(\"P\")"), "{shown}");
}

#[test]
fn v1_global_pretend_still_works_before_an_applet() {
    let shown = v1_prefix_on_applets::show(&["em", "-p", "toolchain", "--setup"]);
    assert!(shown.contains("pretend: true"), "{shown}");
    assert!(shown.contains("Toolchain"), "{shown}");
}

// --- variant 3 ---

#[test]
fn v3_args_conflicts_rejects_pretend_before_toolchain() {
    let err = v3_args_conflicts::parse_err(&["em", "-p", "toolchain", "--setup"]);
    assert_eq!(err, "SubcommandConflict");
}

#[test]
fn v3_args_conflicts_rejects_prefix_before_toolchain() {
    let err = v3_args_conflicts::parse_err(&["em", "--prefix", "/p", "toolchain", "--setup"]);
    assert_eq!(err, "SubcommandConflict");
}

#[test]
fn v3_bare_atom_still_defaults_to_emerge() {
    let app = v3_args_conflicts::parse_ok(&["em", "firefox"]);
    assert!(!app.pretend);
}

// --- variant 2: unique-flags / flatten compile ---

fn cargo_check_ui(src: &str) -> String {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-flatten-global/crate");
    let src_dir = crate_dir.join("src");
    std::fs::create_dir_all(&src_dir).expect("ui crate src dir");
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        r#"[package]
name = "flatten-global-ui"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
usage = { package = "usage-rs", version = "6" }
"#,
    )
    .expect("ui Cargo.toml");
    std::fs::write(src_dir.join("lib.rs"), src).expect("ui lib.rs");

    let target_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-flatten-global/target");
    let output = Command::new(env!("CARGO"))
        .args(["check", "--offline", "--manifest-path"])
        .arg(crate_dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .expect("cargo check ui fixture");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "flatten+global was accepted; it must stay a compile error:\n{text}"
    );
    text
}

#[test]
fn flatten_site_global_is_rejected() {
    let stderr = cargo_check_ui(include_str!("ui/flatten_global.rs"));
    assert!(
        stderr.contains("cannot be combined with `global`"),
        "expected flatten-site global to be rejected, got: {stderr}"
    );
}

#[test]
fn same_type_flatten_compiles_and_unique_flags_hold() {
    let kdl = std::panic::catch_unwind(twin::Twin::to_kdl).unwrap_or_else(|_| {
        panic!("PR 4 must not start: same-type flatten of MergeFlags/RootArg failed unique-flags")
    });
    assert!(
        kdl.contains("flagset merge-flags"),
        "MergeFlags flagset missing:\n{kdl}"
    );
    assert!(
        kdl.contains("flagset root-arg"),
        "RootArg flagset missing:\n{kdl}"
    );
    let json_flags = kdl
        .lines()
        .filter(|l| {
            l.trim_start().starts_with("flag --json")
                || l.trim_start().starts_with("flag \"--json\"")
        })
        .count();
    assert_eq!(
        json_flags, 1,
        "exactly one --json spelling on the root (MergeFlags); got {json_flags}:\n{kdl}"
    );
    assert!(
        kdl.contains("flag --privilege"),
        "privilege redeclare missing:\n{kdl}"
    );
    let _ = twin::Twin::spec();
}

#[test]
fn privilege_redeclare_is_per_command_not_a_flatten() {
    let kdl = twin::Twin::to_kdl();
    let n = kdl.matches("flag --privilege").count();
    assert!(
        n >= 2,
        "privilege must be redeclared on the root and merge applets, got {n}:\n{kdl}"
    );
}

// --- variant 2: proving argv ---

#[test]
fn default_subcommand_routes_firefox_to_emerge() {
    let cli = twin::parse(&["em", "firefox"]);
    assert_eq!(cli.emerge().atoms, ["firefox"]);
}

#[test]
fn topology_global_prefix_before_toolchain() {
    let cli = twin::parse(&["em", "--prefix", "P", "toolchain", "--setup"]);
    assert_eq!(cli.topology.prefix.as_deref(), Some("P"));
    assert!(cli.toolchain().setup);
}

#[test]
fn topology_global_prefix_after_toolchain() {
    let cli = twin::parse(&["em", "toolchain", "--prefix", "P", "--setup"]);
    assert_eq!(cli.topology.prefix.as_deref(), Some("P"));
}

#[test]
fn topology_cascades_into_query_depgraph() {
    let cli = twin::parse(&["em", "query", "depgraph", "--prefix", "P", "zlib"]);
    assert_eq!(cli.topology.prefix.as_deref(), Some("P"));
    assert_eq!(cli.query_depgraph().atom, ["zlib"]);
}

#[test]
fn use_dash_a_is_add_not_ask() {
    let cli = twin::parse(&["em", "use", "-a", "png"]);
    assert_eq!(cli.r#use().add, ["png"]);
    assert!(!cli.merge_flags.ask);
}

#[test]
fn search_dash_a_is_all() {
    let cli = twin::parse(&["em", "search", "-a"]);
    assert!(cli.search().all);
    assert!(!cli.merge_flags.ask);
}

#[test]
fn emerge_dash_a_is_ask() {
    let cli = twin::parse_into(&["em", "emerge", "-a", "pkg"])
        .unwrap_or_else(|e| panic!("try_into {e}"))
        .0;
    assert!(cli.emerge().merge_flags.ask);
    assert_eq!(cli.emerge().atoms, ["pkg"]);
    assert!(!cli.merge_flags.ask);
}

#[test]
fn prefix_ask_then_search_parses_then_try_into_rejects() {
    let cli = twin::parse(&["em", "-a", "search"]);
    assert!(cli.merge_flags.ask);
    assert!(!cli.search().all);
    let err = twin::parse_into(&["em", "-a", "search"]).unwrap_err();
    assert!(
        err.contains("InvalidValue --ask"),
        "try_into must reject prefix --ask before a non-merge applet, got {err}"
    );
}

#[test]
fn worker_root_is_the_worker_flag() {
    let cli = twin::parse(&["em", "__worker", "--root", "/r"]);
    assert_eq!(cli.worker().root, "/r");
    assert!(cli.root_arg.root.is_none());
}

#[test]
fn worker_uses_worker_config_root_not_config_root() {
    let worker = twin::parse(&[
        "em",
        "__worker",
        "--root",
        "/r",
        "--worker-config-root",
        "/c",
    ]);
    assert_eq!(worker.worker().worker_config_root.as_deref(), Some("/c"));
    assert!(worker.topology.config_root.is_none());

    let global = twin::parse(&["em", "__worker", "--root", "/r", "--config-root", "/c"]);
    assert_eq!(global.topology.config_root.as_deref(), Some("/c"));
    assert!(global.worker().worker_config_root.is_none());
}

#[test]
fn worker_quiet_is_the_cli_global() {
    let cli = twin::parse(&["em", "__worker", "--quiet", "--root", "/r"]);
    assert!(cli.quiet);
}

#[test]
fn exclude_value_matching_an_applet_name_is_not_the_search_applet() {
    let cli = twin::parse(&["em", "-X", "search", "-p", "zlib"]);
    assert_eq!(cli.merge_flags.exclude, ["search"]);
    assert!(cli.pretend);
    assert_eq!(cli.emerge().atoms, ["zlib"]);
}

#[test]
fn root_value_named_emerge_is_kept() {
    let cli = twin::parse(&["em", "--root", "emerge", "-p", "zlib"]);
    assert_eq!(cli.root_arg.root.as_deref(), Some("emerge"));
    assert_eq!(cli.emerge().atoms, ["zlib"]);
}

#[test]
fn info_json_and_named_applet() {
    let info = twin::parse(&["em", "--info"]);
    assert!(info.info);
    assert!(info.applet_is_none());

    let json = twin::parse(&["em", "--info", "--json"]);
    assert!(json.info);
    assert!(json.merge_flags.json);
    assert!(json.applet_is_none());

    let r#use = twin::parse(&["em", "--info", "use"]);
    assert!(r#use.info);
    let _ = r#use.r#use();

    assert_eq!(
        twin::parse_err(&["em", "emerge", "--info"]),
        "UnknownFlag --info"
    );

    let firefox = twin::parse(&["em", "--info", "firefox"]);
    assert!(firefox.info);
    assert_eq!(firefox.emerge().atoms, ["firefox"]);
}

#[test]
fn bare_em_is_root_help() {
    let words = harness::argv([] as [&str; 0]);
    let Outcome::Help(printed) =
        harness::outcome(twin::Twin::spec(), &words.words(), twin::Twin::parse_from)
    else {
        panic!("bare em should be MissingArgsHelp");
    };
    assert!(printed.stderr);
    assert_eq!(printed.code, 2);
    assert!(printed.text.contains("query"), "{}", printed.text);
    assert!(printed.text.contains("crossdev"), "{}", printed.text);
    assert!(
        !printed.text.contains("Usage: em emerge"),
        "{}",
        printed.text
    );
    assert!(
        !printed.text.contains("__worker"),
        "hidden worker leaked: {}",
        printed.text
    );
}

#[test]
fn dash_p_alone_parses_without_defaulting_to_emerge() {
    let cli = twin::parse(&["em", "-p"]);
    assert!(cli.pretend);
    assert!(cli.applet_is_none());
}

#[test]
fn asked_help_is_root_help() {
    let words = harness::argv(["--help"]);
    let Outcome::Help(printed) =
        harness::outcome(twin::Twin::spec(), &words.words(), twin::Twin::parse_from)
    else {
        panic!("--help should be Help");
    };
    assert!(!printed.stderr);
    assert_eq!(printed.code, 0);
    assert!(printed.text.contains("query"), "{}", printed.text);
    assert!(
        !printed.text.contains("Usage: em emerge"),
        "{}",
        printed.text
    );
}

#[test]
fn version_is_not_emerge() {
    let words = harness::argv(["--version"]);
    let Outcome::Version(_) =
        harness::outcome(twin::Twin::spec(), &words.words(), twin::Twin::parse_from)
    else {
        panic!("--version should be Version");
    };
}

#[test]
fn prefix_root_then_crossdev_is_try_into_reject() {
    let cli = twin::parse(&["em", "--root", "R", "crossdev", "--setup"]);
    assert_eq!(cli.root_arg.root.as_deref(), Some("R"));
    assert!(cli.is_crossdev());
    let err = twin::parse_into(&["em", "--root", "R", "crossdev", "--setup"]).unwrap_err();
    assert!(
        err.contains("InvalidValue --root"),
        "try_into must reject prefix --root with crossdev, got {err}"
    );
}

#[test]
fn crossdev_root_after_the_applet_is_unknown() {
    assert_eq!(
        twin::parse_err(&["em", "crossdev", "--root", "R", "--setup"]),
        "UnknownFlag --root"
    );
}

#[test]
fn nested_query_depgraph_root_does_not_cascade_without_inner_global() {
    assert_eq!(
        twin::parse_err(&["em", "query", "depgraph", "--root", "R", "zlib"]),
        "UnknownFlag --root"
    );
    let cli = twin::parse(&["em", "query", "--root", "R", "depgraph", "zlib"]);
    assert_eq!(cli.query().root_arg.root.as_deref(), Some("R"));
    assert_eq!(cli.query_depgraph().atom, ["zlib"]);
}

#[test]
fn prefix_root_on_default_emerge() {
    let cli = twin::parse(&["em", "--root", "R", "firefox"]);
    assert_eq!(cli.root_arg.root.as_deref(), Some("R"));
    assert_eq!(cli.emerge().atoms, ["firefox"]);
}

#[test]
fn local_default_missing_and_the_set_trap() {
    let missing = twin::parse(&["em", "--local"]);
    assert_eq!(missing.topology.local.as_deref(), Some(""));
    assert!(missing.applet_is_none());

    let bare = twin::parse(&["em", "--local", "firefox"]);
    assert_eq!(bare.topology.local.as_deref(), Some("firefox"));
    assert!(bare.applet_is_none());

    let path = twin::parse(&["em", "--local", "DIR", "firefox"]);
    assert_eq!(path.topology.local.as_deref(), Some("DIR"));
    assert_eq!(path.emerge().atoms, ["firefox"]);

    let stolen = twin::parse(&["em", "active", "--local", "set"]);
    assert_eq!(stolen.topology.local.as_deref(), Some("set"));
    assert!(stolen.active_command_is_none());

    let ok = twin::parse(&["em", "active", "set", "--local="]);
    assert_eq!(ok.topology.local.as_deref(), Some(""));
    assert!(ok.active_is_set());
}

#[test]
fn helper_hyphen_args_after_double_dash() {
    let cli = twin::parse(&["em", "__helper", "dodoc", "--", "-foo"]);
    assert_eq!(cli.helper().name, "dodoc");
    assert_eq!(cli.helper().args, ["-foo"]);
}

#[test]
fn json_lives_on_merge_flags_both_copies() {
    let prefix = twin::parse(&["em", "--json", "-p", "zlib"]);
    assert!(prefix.merge_flags.json);
    assert!(!prefix.emerge().merge_flags.json);

    let applet = twin::parse(&["em", "emerge", "--json", "pkg"]);
    assert!(!applet.merge_flags.json);
    assert!(applet.emerge().merge_flags.json);
}

#[test]
fn privilege_redeclare_binds_the_copy_that_saw_it() {
    let prefix = twin::parse(&["em", "--privilege", "none", "emerge", "pkg"]);
    assert_eq!(prefix.privilege, mixins::Privilege::None);
    assert_eq!(prefix.emerge().privilege, mixins::Privilege::Auto);

    let applet = twin::parse(&["em", "emerge", "--privilege", "sudo", "pkg"]);
    assert_eq!(applet.privilege, mixins::Privilege::Auto);
    assert_eq!(applet.emerge().privilege, mixins::Privilege::Sudo);
}

#[test]
fn prefix_deep_then_query_is_try_into_reject() {
    let cli = twin::parse(&["em", "--deep", "query", "depgraph", "zlib"]);
    assert!(cli.depgraph_flags.deep);
    let err = twin::parse_into(&["em", "--deep", "query", "depgraph", "zlib"]).unwrap_err();
    assert!(err.contains("InvalidValue emerge-mixin"), "got {err}");
}

#[test]
fn query_depgraph_owns_its_deep() {
    let cli = twin::parse(&["em", "query", "depgraph", "--deep", "zlib"]);
    assert!(!cli.depgraph_flags.deep);
    assert!(cli.query_depgraph().depgraph_flags.deep);
}

#[test]
fn bundled_update_deep_then_query_is_try_into_reject() {
    let cli = twin::parse(&["em", "-uD", "query", "belongs", "/usr/bin/python"]);
    assert!(cli.merge_flags.update);
    assert!(cli.depgraph_flags.deep);
    let err = twin::parse_into(&["em", "-uD", "query", "belongs", "/usr/bin/python"]).unwrap_err();
    assert!(err.contains("InvalidValue emerge-mixin"), "got {err}");
}

#[test]
fn exclude_and_bools_keep_both_copies() {
    let cli = twin::parse(&["em", "-X", "foo", "emerge", "-X", "bar", "pkg"]);
    assert_eq!(cli.merge_flags.exclude, ["foo"]);
    assert_eq!(cli.emerge().merge_flags.exclude, ["bar"]);

    let or_flags = twin::parse(&["em", "-u", "emerge", "-D", "pkg"]);
    assert!(or_flags.merge_flags.update);
    assert!(or_flags.emerge().depgraph_flags.deep);
}

#[test]
fn global_pretend_both_orders() {
    let before = twin::parse(&["em", "-p", "toolchain", "--setup"]);
    assert!(before.pretend);
    let after = twin::parse(&["em", "toolchain", "-p", "--setup"]);
    assert!(after.pretend);
}

#[test]
fn help_tree_marks_hidden_worker() {
    let tree = harness::help_tree(twin::Twin::spec(), Page::Short);
    assert!(tree.contains("__worker (hidden)"), "{tree}");
    assert!(tree.contains("__helper (hidden)"), "{tree}");
}

#[test]
fn root_cascade_fallback_nested_root_and_crossdev_unknown() {
    let q = root_cascade_fallback::parse(&["em", "query", "depgraph", "--root", "R", "zlib"]);
    assert_eq!(q.query_root(), Some("R"));

    let prefix = root_cascade_fallback::parse(&["em", "--root", "R", "firefox"]);
    assert_eq!(prefix.root.as_deref(), Some("R"));

    assert_eq!(
        root_cascade_fallback::parse_err(&["em", "crossdev", "--root", "R", "--setup"]),
        "UnknownFlag --root"
    );
    let leaked = root_cascade_fallback::parse(&["em", "--root", "R", "crossdev", "--setup"]);
    assert_eq!(leaked.root.as_deref(), Some("R"));
    assert!(leaked.is_crossdev());
}
