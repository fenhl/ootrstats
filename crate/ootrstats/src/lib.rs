use {
    std::{
        borrow::Cow,
        collections::HashMap,
        io::prelude::*,
        path::{
            Path,
            PathBuf,
        },
        process::Stdio,
    },
    async_proto::Protocol,
    bytes::Bytes,
    collect_mac::collect,
    directories::BaseDirs,
    if_chain::if_chain,
    itertools::Itertools as _,
    lazy_regex::regex_captures,
    semver::Version,
    serde_json::json,
    tokio::{
        io::AsyncWriteExt as _,
        process::Command,
    },
    wheel::{
        fs,
        traits::{
            AsyncCommandOutputExt as _,
            IoResultExt as _,
        },
    },
};
#[cfg(unix)] use std::env;
#[cfg(windows)] use directories::UserDirs;
#[cfg(target_os = "macos")] use xdg::BaseDirectories;

mod draft;
pub mod websocket;
pub mod worker;

/// install using `wsl --update --pre-release` to get support for the CPU instruction counter and SSH access
pub const WSL: &str = "C:\\Program Files\\WSL\\wsl.exe";

pub type SeedIdx = u16;

#[derive(Clone, Protocol)]
pub enum RandoSetup {
    Normal {
        github_user: String,
        repo: String,
        settings: RandoSettings,
        json_settings: serde_json::Map<String, serde_json::Value>,
        world_counts: bool,
        random_seeds: bool,
    },
    Rsl {
        github_user: String,
        repo: String,
        preset: Option<String>,
    },
}

impl RandoSetup {
    pub fn stats_dir(&self, rando_rev: gix_hash::ObjectId) -> PathBuf {
        match self {
            Self::Normal { github_user, repo, settings, json_settings, world_counts: false, random_seeds: false } if json_settings.is_empty() => Path::new("rando").join(github_user).join(repo).join(rando_rev.to_string()).join(settings.stats_dir()),
            Self::Normal { github_user, repo, settings, .. } => Path::new("rando").join(github_user).join(repo).join(rando_rev.to_string()).join("custom").join(settings.stats_dir()),
            Self::Rsl { github_user, repo, preset: None } => Path::new("rsl").join(github_user).join(repo).join(rando_rev.to_string()),
            Self::Rsl { github_user, repo, preset: Some(preset) } => Path::new("rsl").join(github_user).join(repo).join(rando_rev.to_string()).join(preset),
        }
    }
}

#[derive(Clone, Protocol)]
pub enum RandoSettings {
    Default,
    Preset(String),
    String(String),
    Draft(draft::Spec),
}

impl RandoSettings {
    pub fn stats_dir(&self) -> Cow<'static, Path> {
        match self {
            Self::Default => Path::new("default").into(),
            Self::Preset(preset) => Path::new("preset").join(preset).into(),
            Self::String(settings) => Path::new("settings").join(settings).into(),
            Self::Draft(_) => Path::new("draft").into(), //TODO add a hash of the draft spec as a subdirectory?
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Protocol)]
pub enum OutputMode {
    Normal,
    Bench,
    Patch,
    BenchUncompressed,
}

pub struct RollOutput {
    /// present if the `bench` parameter was set and `perf` output was parsed successfully.
    pub instructions: Result<u64, Bytes>,
    pub rsl_instructions: Result<u64, Bytes>,
    /// `Ok`: spoiler log, `Err`: stderr
    pub log: Result<PathBuf, Bytes>,
    /// `(is_wsl, path)`
    pub patch: Option<(bool, PathBuf)>,
}

#[derive(Debug, thiserror::Error)]
pub enum RollError {
    #[error(transparent)] Draft(#[from] draft::ResolveError),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] ParseInt(#[from] std::num::ParseIntError),
    #[error(transparent)] Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(target_os = "macos")] #[error(transparent)] Xdg(#[from] xdg::BaseDirectoriesError),
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
    #[error("failed to parse `perf` output: {}", String::from_utf8_lossy(.0))]
    PerfSyntax(Vec<u8>),
    #[error("RSL script did not report plando location")]
    PlandoPath(std::process::Output),
    #[error("the RSL script errored\nstdout:\n{}\nstderr:\n{}", String::from_utf8_lossy(&.0.stdout), String::from_utf8_lossy(&.0.stderr))]
    RslScriptExit(std::process::Output),
    #[error("randomizer did not report spoiler log location")]
    SpoilerLogPath(std::process::Output),
}

pub async fn gitdir() -> wheel::Result<Cow<'static, Path>> {
    Ok({
        #[cfg(unix)] {
            if let Some(var) = env::var_os("GITDIR") {
                Cow::Owned(PathBuf::from(var))
            } else if fs::exists("/opt/git").await? {
                Cow::Borrowed(Path::new("/opt/git"))
            } else if let Some(base_dirs) = BaseDirs::new() {
                Cow::Owned(base_dirs.home_dir().join("git"))
            } else {
                Cow::Borrowed(Path::new("/opt/git"))
            }
        }
        #[cfg(windows)] { Cow::Owned(BaseDirs::new().expect("could not determine home dir").home_dir().join("git")) }
    })
}

async fn python() -> Result<PathBuf, RollError> {
    Ok({
        #[cfg(windows)] { UserDirs::new().ok_or(RollError::MissingHomeDir)?.home_dir().join("scoop").join("apps").join("python").join("current").join("python.exe") }
        #[cfg(target_os = "linux")] {
            let python = PathBuf::from("/usr/bin/python3");
            if python.exists() {
                python
            } else {
                PathBuf::from("python3")
            }
        }
        #[cfg(target_os = "macos")] {
            let venv = BaseDirectories::new()?.place_data_file("ootrstats/venv").at_unknown()?;
            if !fs::exists(&venv).await? {
                let system_python = {
                    #[cfg(target_arch = "aarch64")] { PathBuf::from("/opt/homebrew/bin/python3") }
                    #[cfg(target_arch = "x86_64")] { PathBuf::from("/usr/local/bin/python3") }
                };
                Command::new(system_python).arg("-m").arg("venv").arg(&venv).check("python -m venv").await?;
            }
            venv.join("bin").join("python")
        }
    })
}

pub async fn run_rando(wsl_distro: Option<&str>, repo_path: &Path, use_rust_cli: bool, supports_unsalted_seeds: bool, random_seed: bool, settings: &RandoSettings, json_settings: &serde_json::Map<String, serde_json::Value>, world_counts: bool, seed_idx: SeedIdx, output_mode: OutputMode) -> Result<RollOutput, RollError> {
    let mut resolved_settings = collect![as HashMap<_, _>:
        Cow::Borrowed("create_spoiler") => json!(true),
        Cow::Borrowed("create_cosmetics_log") => json!(matches!(output_mode, OutputMode::Bench | OutputMode::BenchUncompressed)),
        Cow::Borrowed("create_patch_file") => json!(output_mode == OutputMode::Patch),
        Cow::Borrowed("create_uncompressed_rom") => json!(output_mode == OutputMode::BenchUncompressed),
        Cow::Borrowed("create_compressed_rom") => json!(output_mode == OutputMode::Bench),
    ];
    if supports_unsalted_seeds && !random_seed {
        resolved_settings.insert(Cow::Borrowed("salt_seed"), json!(false));
    }
    resolved_settings.extend(json_settings.iter().map(|(name, value)| (Cow::<str>::Borrowed(name), value.clone())));
    if world_counts {
        resolved_settings.insert(Cow::Borrowed("world_count"), json!(seed_idx + 1));
    }
    let mut cmd_name;
    let mut cmd;
    if use_rust_cli {
        cmd_name = repo_path.join("target").join("release").join("ootr-cli").display().to_string();
        cmd = if let OutputMode::Bench | OutputMode::BenchUncompressed = output_mode {
            #[cfg(any(target_os = "linux", target_os = "windows"))] {
                let mut cmd = {
                    #[cfg(target_os = "linux")] {
                        cmd_name = format!("perf stat {cmd_name}");
                        Command::new("perf")
                    }
                    #[cfg(target_os = "windows")] {
                        cmd_name = format!("{WSL} perf stat {cmd_name}");
                        let mut cmd = Command::new(WSL);
                        if let Some(wsl_distro) = wsl_distro {
                            cmd.arg("--distribution");
                            cmd.arg(wsl_distro);
                        }
                        // install using `apt-get install linux-tools-generic` and symlink from `/usr/lib/linux-tools/*-generic/perf`
                        cmd.arg("perf");
                        cmd
                    }
                };
                cmd.arg("stat");
                cmd.arg("--event=instructions:u");
                cmd.arg("target/release/ootr-cli");
                cmd
            }
            #[cfg(target_os = "macos")] {
                cmd_name = format!("time {cmd_name}");
                let mut cmd = Command::new("/usr/bin/time");
                cmd.arg("-l");
                cmd.arg("target/release/ootr-cli");
                cmd
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))] {
                unimplemented!("`bench` subcommand not yet implemented for this OS")
            }
        } else {
            #[cfg(target_os = "windows")] {
                cmd_name = repo_path.join("target").join("release").join("ootr-cli.exe").display().to_string();
                Command::new(repo_path.join("target").join("release").join("ootr-cli.exe"))
            }
            #[cfg(not(target_os = "windows"))] { Command::new(repo_path.join("target").join("release").join("ootr-cli")) }
        };
        cmd.arg("--no-log");
        match settings {
            RandoSettings::Default => {}
            RandoSettings::Preset(preset) => {
                cmd.arg("--settings-preset");
                cmd.arg(preset);
            }
            RandoSettings::String(settings) => {
                cmd.arg("--settings-string");
                cmd.arg(settings);
            }
            RandoSettings::Draft(spec) => resolved_settings.extend(spec.complete_randomly()?),
        }
    } else {
        let python = python().await?;
        cmd_name = python.display().to_string();
        cmd = if let OutputMode::Bench | OutputMode::BenchUncompressed = output_mode {
            #[cfg(any(target_os = "linux", target_os = "windows"))] {
                let mut cmd = {
                    #[cfg(target_os = "linux")] {
                        cmd_name = format!("perf stat {cmd_name}");
                        Command::new("perf")
                    }
                    #[cfg(target_os = "windows")] {
                        cmd_name = format!("{WSL} perf stat python3");
                        let mut cmd = Command::new(WSL);
                        if let Some(wsl_distro) = wsl_distro {
                            cmd.arg("--distribution");
                            cmd.arg(wsl_distro);
                        }
                        // install using `apt-get install linux-tools-generic` and symlink from `/usr/lib/linux-tools/*-generic/perf`
                        cmd.arg("perf");
                        cmd
                    }
                };
                cmd.arg("stat");
                cmd.arg("--event=instructions:u");
                #[cfg(target_os = "linux")] cmd.arg(&python);
                #[cfg(target_os = "windows")] cmd.arg("python3");
                cmd
            }
            #[cfg(target_os = "macos")] {
                cmd_name = format!("time {cmd_name}");
                let mut cmd = Command::new("/usr/bin/time");
                cmd.arg("-l");
                cmd.arg(&python);
                cmd
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))] {
                unimplemented!("`bench` subcommand not yet implemented for this OS")
            }
        } else {
            Command::new(&python)
        };
        cmd.arg("-c");
        cmd.arg("import OoTRandomizer; OoTRandomizer.start()"); // called this way to allow mypyc optimization to work
        cmd.arg("--no_log");
        match settings {
            RandoSettings::Default => {}
            RandoSettings::Preset(preset) => {
                cmd.arg("--settings_preset");
                cmd.arg(preset);
            }
            RandoSettings::String(settings) => {
                cmd.arg("--settings_string");
                cmd.arg(settings);
            }
            RandoSettings::Draft(spec) => resolved_settings.extend(spec.complete_randomly()?),
        }
    }
    cmd.arg("--settings=-");
    if !random_seed {
        cmd.arg(format!("--seed=ootrstats{seed_idx}"));
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    cmd.current_dir(repo_path);
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().at_command(cmd_name.clone())?;
    child.stdin.as_mut().expect("configured").write_all(&serde_json::to_vec(&resolved_settings)?).await.at_command(cmd_name.clone())?;
    let output = child.wait_with_output().await.at_command(cmd_name.clone())?;
    let stderr = BufRead::lines(&*output.stderr).try_collect::<_, Vec<_>, _>().at_command(cmd_name)?;
    if output.status.success() {
        if let Some(distribution_file_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Copied distribution file to: ")) {
            if cfg!(target_os = "windows") && matches!(output_mode, OutputMode::Bench | OutputMode::BenchUncompressed) {
                let mut cmd = Command::new(WSL);
                if let Some(wsl_distro) = wsl_distro {
                    cmd.arg("--distribution");
                    cmd.arg(wsl_distro);
                }
                cmd.arg("rm");
                cmd.arg(distribution_file_path);
                cmd.check("wsl rm").await?;
            } else {
                fs::remove_file(distribution_file_path).await?;
            }
        }
        if let Some(uncompressed_rom_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Saving Uncompressed ROM: ")) {
            fs::remove_file(repo_path.join("Output").join(uncompressed_rom_path)).await?;
        }
        if let Some(compressed_rom_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Created compressed ROM at: ")) {
            if cfg!(target_os = "windows") && matches!(output_mode, OutputMode::Bench | OutputMode::BenchUncompressed) {
                let mut cmd = Command::new(WSL);
                if let Some(wsl_distro) = wsl_distro {
                    cmd.arg("--distribution");
                    cmd.arg(wsl_distro);
                }
                cmd.arg("rm");
                cmd.arg(compressed_rom_path);
                cmd.check("wsl rm").await?;
            } else {
                fs::remove_file(compressed_rom_path).await?;
            }
        }
        if let Some(cosmetics_log_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Created cosmetic log at: ")) {
            if cfg!(target_os = "windows") && matches!(output_mode, OutputMode::Bench | OutputMode::BenchUncompressed) {
                let mut cmd = Command::new(WSL);
                if let Some(wsl_distro) = wsl_distro {
                    cmd.arg("--distribution");
                    cmd.arg(wsl_distro);
                }
                cmd.arg("rm");
                cmd.arg(cosmetics_log_path);
                cmd.check("wsl rm").await?;
            } else {
                fs::remove_file(cosmetics_log_path).await?;
            }
        }
    }
    Ok(RollOutput {
        instructions: if let OutputMode::Bench | OutputMode::BenchUncompressed = output_mode {
            #[cfg(any(target_os = "linux", target_os = "windows"))] {
                if_chain! {
                    if let Some(instructions_line) = stderr.iter().rev().find(|line| line.contains("instructions:u"));
                    if let Some((_, instructions)) = regex_captures!("^ *([0-9,.]+) +instructions:u", instructions_line);
                    then {
                        Ok(instructions.chars().filter(|&c| c != ',' && c != '.').collect::<String>().parse()?)
                    } else {
                        Err(output.stderr.clone().into())
                    }
                }
            }
            #[cfg(target_os = "macos")] {
                if_chain! {
                    if let Some(instructions_line) = stderr.iter().rev().find(|line| line.contains("instructions retired"));
                    if let Some((_, instructions)) = regex_captures!("^ *([0-9]+) +instructions retired", instructions_line);
                    then {
                        Ok(instructions.parse()?)
                    } else {
                        Err(output.stderr.clone().into())
                    }
                }
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))] {
                unimplemented!("`bench` subcommand not yet implemented for this OS")
            }
        } else {
            Err(Bytes::from_static(b"output mode"))
        },
        patch: if output.status.success() {
            if let Some(patch_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Created patch file archive at: ")) {
                Some((cfg!(target_os = "windows") && matches!(output_mode, OutputMode::Bench | OutputMode::BenchUncompressed), PathBuf::from(patch_path)))
            } else if let Some(patch_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Creating Patch File: ")) {
                Some((false, repo_path.join("Output").join(patch_path)))
            } else {
                None
            }
        } else {
            None
        },
        log: if output.status.success() {
            Ok(repo_path.join("Output").join(stderr.iter().rev().find_map(|line| line.strip_prefix("Created spoiler log at: ")).ok_or_else(|| RollError::SpoilerLogPath(output))?))
        } else {
            Err(output.stderr.into())
        },
        rsl_instructions: Ok(0),
    })
}

pub async fn run_rsl(#[cfg_attr(not(target_os = "windows"), allow(unused))] wsl_distro: Option<&str>, repo_path: &Path, use_rust_cli: bool, supports_unsalted_seeds: bool, random_seed: bool, preset: Option<&str>, seed_idx: SeedIdx, output_mode: OutputMode) -> Result<RollOutput, RollError> {
    let python = python().await?;
    #[cfg_attr(not(target_os = "windows"), allow(unused_mut))] let mut cmd_name = python.display().to_string();
    let rsl_version = Command::new(&python)
        .arg("-c")
        .arg("import rslversion; print(rslversion.__version__)")
        .current_dir(repo_path)
        .check(cmd_name.clone()).await?
        .stdout;
    let rsl_version = String::from_utf8(rsl_version)?;
    let (supports_plando_filename_base, supports_seed, supports_no_salt) = if let Some((_, major, minor, patch, supplementary)) = regex_captures!(r"^([0-9]+)\.([0-9]+)\.([0-9]+) Fenhl-([0-9]+)$", &rsl_version.trim()) {
        let rsl_version = (Version::new(major.parse()?, minor.parse()?, patch.parse()?), supplementary.parse()?);
        (rsl_version >= (Version::new(2, 8, 2), 0), rsl_version >= (Version::new(2, 8, 2), 3), rsl_version >= (Version::new(2, 8, 2), 3))
    } else if let Some((_, major, minor, patch, supplementary)) = regex_captures!(r"^([0-9]+)\.([0-9]+)\.([0-9]+) devmvp-([0-9]+)$", &rsl_version.trim()) {
        let rsl_version = (Version::new(major.parse()?, minor.parse()?, patch.parse()?), supplementary.parse()?);
        (rsl_version >= (Version::new(2, 6, 3), 4), false, false)
    } else {
        (rsl_version.parse::<Version>().is_ok_and(|rsl_version| rsl_version >= Version::new(2, 8, 2)), false, false)
    };
    let mut cmd = if let OutputMode::Bench | OutputMode::BenchUncompressed = output_mode {
        #[cfg(any(target_os = "linux", target_os = "windows"))] {
            let mut cmd = {
                #[cfg(target_os = "linux")] {
                    cmd_name = format!("perf stat {cmd_name}");
                    Command::new("perf")
                }
                #[cfg(target_os = "windows")] {
                    cmd_name = format!("{WSL} perf stat python3");
                    let mut cmd = Command::new(WSL);
                    if let Some(wsl_distro) = wsl_distro {
                        cmd.arg("--distribution");
                        cmd.arg(wsl_distro);
                    }
                    // install using `apt-get install linux-tools-generic` and symlink from `/usr/lib/linux-tools/*-generic/perf`
                    cmd.arg("perf");
                    cmd
                }
            };
            cmd.arg("stat");
            cmd.arg("--event=instructions:u");
            #[cfg(target_os = "linux")] cmd.arg(&python);
            #[cfg(target_os = "windows")] cmd.arg("python3");
            cmd
        }
        #[cfg(target_os = "macos")] {
            cmd_name = format!("time {cmd_name}");
            let mut cmd = Command::new("/usr/bin/time");
            cmd.arg("-l");
            cmd.arg(&python);
            cmd
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))] {
            unimplemented!("`bench` subcommand not yet implemented for this OS")
        }
    } else {
        Command::new(&python)
    };
    cmd.arg("RandomSettingsGenerator.py");
    cmd.arg("--no_log_errors");
    cmd.arg("--no_seed");
    if supports_plando_filename_base {
        cmd.arg(format!("--plando_filename_base=ootrstats_{seed_idx}"));
    }
    if supports_seed && !random_seed {
        cmd.arg(format!("--seed=ootrstats{seed_idx}"));
        if supports_no_salt {
            cmd.arg("--no_salt");
        }
    }
    if let Some(preset) = preset {
        cmd.arg(format!("--override=weights/{preset}_override.json"));
    }
    cmd.current_dir(repo_path);
    let output = cmd.output().await.at_command(cmd_name.clone())?;
    let stderr = BufRead::lines(&*output.stderr).try_collect::<_, Vec<_>, _>().at_command(cmd_name.clone())?;
    if output.status.success() || output.status.code() == Some(3) {
        let stdout = BufRead::lines(&*output.stdout).try_collect::<_, Vec<_>, _>().at_command(cmd_name)?;
        let plando_path = Path::new("data").join(stdout.iter().rev().find_map(|line| line.strip_prefix("Plando File: ")).ok_or_else(|| RollError::SpoilerLogPath(output.clone()))?);
        let mut roll_output = run_rando(wsl_distro, &repo_path.join("randomizer"), use_rust_cli, supports_unsalted_seeds, random_seed, &RandoSettings::Default, &collect![
            format!("enable_distribution_file") => json!(true),
            format!("distribution_file") => json!(plando_path),
        ], false, seed_idx, output_mode).await?;
        fs::remove_file(plando_path).await?;
        roll_output.rsl_instructions = if let OutputMode::Bench | OutputMode::BenchUncompressed = output_mode {
            #[cfg(any(target_os = "linux", target_os = "windows"))] {
                if_chain! {
                    if let Some(instructions_line) = stderr.iter().rev().find(|line| line.contains("instructions:u"));
                    if let Some((_, instructions)) = regex_captures!("^ *([0-9,.]+) +instructions:u", instructions_line);
                    then {
                        Ok(instructions.chars().filter(|&c| c != ',' && c != '.').collect::<String>().parse()?)
                    } else {
                        Err(output.stderr.clone().into())
                    }
                }
            }
            #[cfg(target_os = "macos")] {
                if_chain! {
                    if let Some(instructions_line) = stderr.iter().rev().find(|line| line.contains("instructions retired"));
                    if let Some((_, instructions)) = regex_captures!("^ *([0-9]+) +instructions retired", instructions_line);
                    then {
                        Ok(instructions.parse()?)
                    } else {
                        Err(output.stderr.clone().into())
                    }
                }
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))] {
                unimplemented!("`bench` subcommand not yet implemented for this OS")
            }
        } else {
            Err(Bytes::from_static(b"output mode"))
        };
        Ok(roll_output)
    } else {
        Err(RollError::RslScriptExit(output))
    }
}
