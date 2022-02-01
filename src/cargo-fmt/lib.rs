use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self};
use std::iter::FromIterator;
use std::path::{Path, PathBuf};
use std::process::Command;

const SUCCESS: i32 = 0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verbosity {
    Verbose,
    Normal,
    Quiet,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CargoFmtStrategy {
    /// Format every packages and dependencies.
    All,
    /// Format packages that are specified by the command line argument.
    Some(Vec<String>),
    /// Format the root packages only.
    Root,
}

/// Based on the specified `CargoFmtStrategy`, returns a set of main source files.
pub fn get_targets(
    strategy: &CargoFmtStrategy,
    manifest_path: Option<&Path>,
) -> Result<BTreeSet<Target>, io::Error> {
    let mut targets = BTreeSet::new();

    match *strategy {
        CargoFmtStrategy::Root => get_targets_root_only(manifest_path, &mut targets)?,
        CargoFmtStrategy::All => {
            get_targets_recursive(manifest_path, &mut targets, &mut BTreeSet::new())?
        }
        CargoFmtStrategy::Some(ref hitlist) => {
            get_targets_with_hitlist(manifest_path, hitlist, &mut targets)?
        }
    }

    if targets.is_empty() {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "Failed to find targets".to_owned(),
        ))
    } else {
        Ok(targets)
    }
}

fn get_targets_root_only(
    manifest_path: Option<&Path>,
    targets: &mut BTreeSet<Target>,
) -> Result<(), io::Error> {
    let metadata = get_cargo_metadata(manifest_path)?;
    let workspace_root_path = PathBuf::from(&metadata.workspace_root).canonicalize()?;
    let (in_workspace_root, current_dir_manifest) = if let Some(target_manifest) = manifest_path {
        (
            workspace_root_path == target_manifest,
            target_manifest.canonicalize()?,
        )
    } else {
        let current_dir = env::current_dir()?.canonicalize()?;
        (
            workspace_root_path == current_dir,
            current_dir.join("Cargo.toml"),
        )
    };

    let package_targets = match metadata.packages.len() {
        1 => metadata.packages.into_iter().next().unwrap().targets,
        _ => metadata
            .packages
            .into_iter()
            .filter(|p| {
                in_workspace_root
                    || PathBuf::from(&p.manifest_path)
                        .canonicalize()
                        .unwrap_or_default()
                        == current_dir_manifest
            })
            .flat_map(|p| p.targets)
            .collect(),
    };

    for target in package_targets {
        targets.insert(Target::from_target(&target));
    }

    Ok(())
}

fn get_targets_recursive(
    manifest_path: Option<&Path>,
    targets: &mut BTreeSet<Target>,
    visited: &mut BTreeSet<String>,
) -> Result<(), io::Error> {
    let metadata = get_cargo_metadata(manifest_path)?;
    for package in &metadata.packages {
        add_targets(&package.targets, targets);

        // Look for local dependencies using information available since cargo v1.51
        // It's theoretically possible someone could use a newer version of rustfmt with
        // a much older version of `cargo`, but we don't try to explicitly support that scenario.
        // If someone reports an issue with path-based deps not being formatted, be sure to
        // confirm their version of `cargo` (not `cargo-fmt`) is >= v1.51
        // https://github.com/rust-lang/cargo/pull/8994
        for dependency in &package.dependencies {
            if dependency.path.is_none() || visited.contains(&dependency.name) {
                continue;
            }

            let manifest_path = PathBuf::from(dependency.path.as_ref().unwrap()).join("Cargo.toml");
            if manifest_path.exists()
                && !metadata
                    .packages
                    .iter()
                    .any(|p| p.manifest_path.eq(&manifest_path))
            {
                visited.insert(dependency.name.to_owned());
                get_targets_recursive(Some(&manifest_path), targets, visited)?;
            }
        }
    }

    Ok(())
}

fn get_targets_with_hitlist(
    manifest_path: Option<&Path>,
    hitlist: &[String],
    targets: &mut BTreeSet<Target>,
) -> Result<(), io::Error> {
    let metadata = get_cargo_metadata(manifest_path)?;
    let mut workspace_hitlist: BTreeSet<&String> = BTreeSet::from_iter(hitlist);

    for package in metadata.packages {
        if workspace_hitlist.remove(&package.name) {
            for target in package.targets {
                targets.insert(Target::from_target(&target));
            }
        }
    }

    if workspace_hitlist.is_empty() {
        Ok(())
    } else {
        let package = workspace_hitlist.iter().next().unwrap();
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("package `{}` is not a member of the workspace", package),
        ))
    }
}

fn add_targets(target_paths: &[cargo_metadata::Target], targets: &mut BTreeSet<Target>) {
    for target in target_paths {
        targets.insert(Target::from_target(target));
    }
}

fn run_rustfmt(
    rustfmt_path: &Path,
    targets: &BTreeSet<Target>,
    fmt_args: &[String],
    verbosity: Verbosity,
) -> Result<i32, io::Error> {
    let by_edition = targets
        .iter()
        .inspect(|t| {
            if verbosity == Verbosity::Verbose {
                println!("[{} ({})] {:?}", t.kind, t.edition, t.path)
            }
        })
        .fold(BTreeMap::new(), |mut h, t| {
            h.entry(&t.edition).or_insert_with(Vec::new).push(&t.path);
            h
        });

    let mut status = vec![];
    for (edition, files) in by_edition {
        let stdout = if verbosity == Verbosity::Quiet {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::inherit()
        };

        if verbosity == Verbosity::Verbose {
            print!("rustfmt");
            print!(" --edition {}", edition);
            fmt_args.iter().for_each(|f| print!(" {}", f));
            files.iter().for_each(|f| print!(" {}", f.display()));
            println!();
        }

        let mut child = Command::new(rustfmt_path)
            .stdout(stdout)
            .args(files)
            .args(&["--edition", edition])
            .args(fmt_args)
            .spawn()
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => io::Error::new(
                    io::ErrorKind::Other,
                    "Could not run rustfmt, please make sure it is in your PATH.",
                ),
                _ => e,
            })?;

        status.push(child.wait()?);
    }

    Ok(status
        .iter()
        .filter_map(|s| if s.success() { None } else { s.code() })
        .next()
        .unwrap_or(SUCCESS))
}

fn get_cargo_metadata(manifest_path: Option<&Path>) -> Result<cargo_metadata::Metadata, io::Error> {
    let mut cmd = cargo_metadata::MetadataCommand::new();
    cmd.no_deps();
    if let Some(manifest_path) = manifest_path {
        cmd.manifest_path(manifest_path);
    }
    cmd.other_options(vec![String::from("--offline")]);

    match cmd.exec() {
        Ok(metadata) => Ok(metadata),
        Err(_) => {
            cmd.other_options(vec![]);
            match cmd.exec() {
                Ok(metadata) => Ok(metadata),
                Err(error) => Err(io::Error::new(io::ErrorKind::Other, error.to_string())),
            }
        }
    }
}

#[must_use]
pub fn format_crate(
    rustfmt_path: &Path,
    verbosity: Verbosity,
    strategy: &CargoFmtStrategy,
    rustfmt_args: Vec<String>,
    manifest_path: Option<&Path>,
) -> Result<i32, io::Error> {
    let targets = get_targets(strategy, manifest_path)?;

    // Currently only bin and lib files get formatted.
    run_rustfmt(rustfmt_path, &targets, &rustfmt_args, verbosity)
}

/// Target uses a `path` field for equality and hashing.
#[derive(Debug)]
pub struct Target {
    /// A path to the main source file of the target.
    pub path: PathBuf,
    /// A kind of target (e.g., lib, bin, example, ...).
    pub kind: String,
    /// Rust edition for this target.
    pub edition: String,
}

impl Target {
    pub fn from_target(target: &cargo_metadata::Target) -> Self {
        let path = PathBuf::from(&target.src_path);
        let canonicalized = fs::canonicalize(&path).unwrap_or(path);

        Target {
            path: canonicalized,
            kind: target.kind[0].clone(),
            edition: target.edition.clone(),
        }
    }
}

impl PartialEq for Target {
    fn eq(&self, other: &Target) -> bool {
        self.path == other.path
    }
}

impl PartialOrd for Target {
    fn partial_cmp(&self, other: &Target) -> Option<Ordering> {
        Some(self.path.cmp(&other.path))
    }
}

impl Ord for Target {
    fn cmp(&self, other: &Target) -> Ordering {
        self.path.cmp(&other.path)
    }
}

impl Eq for Target {}

impl Hash for Target {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
    }
}
