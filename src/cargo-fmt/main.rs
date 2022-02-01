// Inspired by Paul Woolcock's cargo-fmt (https://github.com/pwoolcoc/cargo-fmt/).

#![deny(warnings)]
#![allow(clippy::match_like_matches_macro)]

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::str;

use cargo_fmt::{CargoFmtStrategy, Verbosity};
use structopt::StructOpt;

#[path = "lib.rs"]
mod cargo_fmt;

#[path = "test/mod.rs"]
#[cfg(test)]
mod cargo_fmt_tests;

#[derive(StructOpt, Debug)]
#[structopt(
    bin_name = "cargo fmt",
    about = "This utility formats all bin and lib files of \
             the current crate using rustfmt."
)]
pub struct Opts {
    /// No output printed to stdout
    #[structopt(short = "q", long = "quiet")]
    quiet: bool,

    /// Use verbose output
    #[structopt(short = "v", long = "verbose")]
    verbose: bool,

    /// Print rustfmt version and exit
    #[structopt(long = "version")]
    version: bool,

    /// Specify package to format
    #[structopt(short = "p", long = "package", value_name = "package")]
    packages: Vec<String>,

    /// Specify path to Cargo.toml
    #[structopt(long = "manifest-path", value_name = "manifest-path")]
    manifest_path: Option<String>,

    /// Specify message-format: short|json|human
    #[structopt(long = "message-format", value_name = "message-format")]
    message_format: Option<String>,

    /// Options passed to rustfmt
    // 'raw = true' to make `--` explicit.
    #[structopt(name = "rustfmt_options", raw(true))]
    rustfmt_options: Vec<String>,

    /// Format all packages, and also their local path-based dependencies
    #[structopt(long = "all")]
    format_all: bool,

    /// Run rustfmt in check mode
    #[structopt(long = "check")]
    check: bool,
}

fn main() {
    let exit_status = execute();
    std::io::stdout().flush().unwrap();
    std::process::exit(exit_status);
}

const SUCCESS: i32 = 0;
const FAILURE: i32 = 1;

fn execute() -> i32 {
    // Drop extra `fmt` argument provided by `cargo`.
    let mut found_fmt = false;
    let args = env::args().filter(|x| {
        if found_fmt {
            true
        } else {
            found_fmt = x == "fmt";
            x != "fmt"
        }
    });

    let opts = Opts::from_iter(args);

    let verbosity = match (opts.verbose, opts.quiet) {
        (false, false) => Verbosity::Normal,
        (false, true) => Verbosity::Quiet,
        (true, false) => Verbosity::Verbose,
        (true, true) => {
            print_usage_to_stderr("quiet mode and verbose mode are not compatible");
            return FAILURE;
        }
    };

    if opts.version {
        return handle_command_status(get_rustfmt_info(&[String::from("--version")]));
    }
    if opts.rustfmt_options.iter().any(|s| {
        ["--print-config", "-h", "--help", "-V", "--version"].contains(&s.as_str())
            || s.starts_with("--help=")
            || s.starts_with("--print-config=")
    }) {
        return handle_command_status(get_rustfmt_info(&opts.rustfmt_options));
    }

    let strategy = CargoFmtStrategy::from_opts(&opts);
    let mut rustfmt_args = opts.rustfmt_options;
    if opts.check {
        let check_flag = "--check";
        if !rustfmt_args.iter().any(|o| o == check_flag) {
            rustfmt_args.push(check_flag.to_owned());
        }
    }
    if let Some(message_format) = opts.message_format {
        if let Err(msg) = convert_message_format_to_rustfmt_args(&message_format, &mut rustfmt_args)
        {
            print_usage_to_stderr(&msg);
            return FAILURE;
        }
    }

    let manifest_path = match opts.manifest_path {
        Some(specified_manifest_path) => {
            if !specified_manifest_path.ends_with("Cargo.toml") {
                print_usage_to_stderr("the manifest-path must be a path to a Cargo.toml file");
                return FAILURE;
            }
            Some(PathBuf::from(specified_manifest_path))
        }
        None => None,
    };
    let status = cargo_fmt::format_crate(
        &rustfmt_path(),
        verbosity,
        &strategy,
        rustfmt_args,
        manifest_path.as_deref(),
    );
    handle_command_status(status)
}

fn rustfmt_path() -> PathBuf {
    match env::var_os("RUSTFMT") {
        None => "rustfmt".into(),
        Some(p) => p.into(),
    }
}

fn convert_message_format_to_rustfmt_args(
    message_format: &str,
    rustfmt_args: &mut Vec<String>,
) -> Result<(), String> {
    let mut contains_emit_mode = false;
    let mut contains_check = false;
    let mut contains_list_files = false;
    for arg in rustfmt_args.iter() {
        if arg.starts_with("--emit") {
            contains_emit_mode = true;
        }
        if arg == "--check" {
            contains_check = true;
        }
        if arg == "-l" || arg == "--files-with-diff" {
            contains_list_files = true;
        }
    }
    match message_format {
        "short" => {
            if !contains_list_files {
                rustfmt_args.push(String::from("-l"));
            }
            Ok(())
        }
        "json" => {
            if contains_emit_mode {
                return Err(String::from(
                    "cannot include --emit arg when --message-format is set to json",
                ));
            }
            if contains_check {
                return Err(String::from(
                    "cannot include --check arg when --message-format is set to json",
                ));
            }
            rustfmt_args.push(String::from("--emit"));
            rustfmt_args.push(String::from("json"));
            Ok(())
        }
        "human" => Ok(()),
        _ => {
            return Err(format!(
                "invalid --message-format value: {}. Allowed values are: short|json|human",
                message_format
            ));
        }
    }
}

fn print_usage_to_stderr(reason: &str) {
    eprintln!("{}", reason);
    let app = Opts::clap();
    app.after_help("")
        .write_help(&mut io::stderr())
        .expect("failed to write to stderr");
}

fn handle_command_status(status: Result<i32, io::Error>) -> i32 {
    match status {
        Err(e) => {
            print_usage_to_stderr(&e.to_string());
            FAILURE
        }
        Ok(status) => status,
    }
}

fn get_rustfmt_info(args: &[String]) -> Result<i32, io::Error> {
    let mut command = Command::new(rustfmt_path())
        .stdout(std::process::Stdio::inherit())
        .args(args)
        .spawn()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => io::Error::new(
                io::ErrorKind::Other,
                "Could not run rustfmt, please make sure it is in your PATH.",
            ),
            _ => e,
        })?;
    let result = command.wait()?;
    if result.success() {
        Ok(SUCCESS)
    } else {
        Ok(result.code().unwrap_or(SUCCESS))
    }
}

impl CargoFmtStrategy {
    pub fn from_opts(opts: &Opts) -> CargoFmtStrategy {
        match (opts.format_all, opts.packages.is_empty()) {
            (false, true) => CargoFmtStrategy::Root,
            (true, _) => CargoFmtStrategy::All,
            (false, false) => CargoFmtStrategy::Some(opts.packages.clone()),
        }
    }
}
