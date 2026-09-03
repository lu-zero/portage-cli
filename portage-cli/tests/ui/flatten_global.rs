//! Compile-fail lock: flatten-site `global` is not valid.
use usage::{Args, Cli};

#[derive(Args)]
struct RootArg {
    #[usage(long, value_name = "PATH")]
    root: Option<String>,
}

#[derive(Cli)]
#[usage(bin = "em")]
struct App {
    #[usage(flatten, global)]
    root_arg: RootArg,
}
