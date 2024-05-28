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

pub mod websocket;
pub mod worker;

/// install using `wsl --update --pre-release` to get support for the CPU instruction counter and SSH access
pub const WSL: &str = "C:\\Program Files\\WSL\\wsl.exe";

pub type SeedIdx = u16;

#[derive(Clone, Protocol)]
pub enum RandoSetup {
    Normal {
        github_user: String,
        settings: RandoSettings,
        json_settings: serde_json::Map<String, serde_json::Value>,
        world_counts: bool,
    },
    Rsl {
        github_user: String,
    },
}

impl RandoSetup {
    pub fn stats_dir(&self, rando_rev: git2::Oid) -> PathBuf {
        match self {
            Self::Normal { github_user, settings, json_settings, world_counts: false } if json_settings.is_empty() => Path::new("rando").join(github_user).join(rando_rev.to_string()).join(settings.stats_dir()),
            Self::Normal { github_user, settings, .. } => Path::new("rando").join(github_user).join(rando_rev.to_string()).join("custom").join(settings.stats_dir()),
            Self::Rsl { github_user } => Path::new("rsl").join(github_user).join(rando_rev.to_string()),
        }
    }
}

#[derive(Clone, Protocol)]
pub enum RandoSettings {
    Default,
    Preset(String),
    String(String),
}

impl RandoSettings {
    pub fn stats_dir(&self) -> Cow<'static, Path> {
        match self {
            Self::Default => Path::new("default").into(),
            Self::Preset(preset) => Path::new("preset").join(preset).into(),
            Self::String(settings) => Path::new("settings").join(settings).into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Protocol)]
pub enum OutputMode {
    Normal,
    Bench,
    Patch,
}

pub struct RollOutput {
    /// present if the `bench` parameter was set and `perf` output was parsed successfully.
    pub instructions: Result<u64, Bytes>,
    /// `Ok`: spoiler log, `Err`: stderr
    pub log: Result<PathBuf, Bytes>,
    /// `(is_wsl, path)`
    pub patch: Option<(bool, PathBuf)>,
}

#[derive(Debug, thiserror::Error)]
pub enum RollError {
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] ParseInt(#[from] std::num::ParseIntError),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
    #[error("failed to parse `perf` output: {}", String::from_utf8_lossy(.0))]
    PerfSyntax(Vec<u8>),
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
            } else {
                if_chain! {
                    if let Some(base_dirs) = BaseDirs::new();
                    let path = base_dirs.home_dir().join("git");
                    if fs::exists(&path).await?;
                    then {
                        Cow::Owned(path)
                    } else {
                        Cow::Borrowed(Path::new("/opt/git"))
                    }
                }
            }
        }
        #[cfg(windows)] { Cow::Owned(BaseDirs::new().expect("could not determine home dir").home_dir().join("git")) }
    })
}

fn python() -> Result<PathBuf, RollError> {
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
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))] { PathBuf::from("/opt/homebrew/bin/python3") }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))] { PathBuf::from("/usr/local/bin/python3") }
    })
}

pub async fn run_rando(base_rom_path: &Path, repo_path: &Path, settings: &RandoSettings, json_settings: &serde_json::Map<String, serde_json::Value>, world_counts: bool, seed_idx: SeedIdx, output_mode: OutputMode) -> Result<RollOutput, RollError> {
    let mut resolved_settings = collect![as HashMap<_, _>:
        Cow::Borrowed("rom") => json!(base_rom_path),
        Cow::Borrowed("create_spoiler") => json!(true),
        Cow::Borrowed("create_cosmetics_log") => json!(output_mode == OutputMode::Bench),
        Cow::Borrowed("create_patch_file") => json!(output_mode == OutputMode::Patch),
        Cow::Borrowed("create_compressed_rom") => json!(output_mode == OutputMode::Bench),
    ];
    resolved_settings.extend(json_settings.iter().map(|(name, value)| (Cow::<str>::Borrowed(name), value.clone())));
    if world_counts {
        resolved_settings.insert(Cow::Borrowed("world_count"), json!(seed_idx + 1));
    }
    let python = python()?;
    #[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(unused_mut))] let mut cmd_name = python.display().to_string();
    let mut cmd = if let OutputMode::Bench = output_mode {
        #[cfg(any(target_os = "linux", target_os = "windows"))] {
            let mut cmd = {
                #[cfg(target_os = "linux")] {
                    cmd_name = format!("perf stat {cmd_name}");
                    Command::new("perf")
                }
                #[cfg(target_os = "windows")] {
                    cmd_name = format!("{WSL} perf stat python3");
                    let mut cmd = Command::new(WSL);
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
        #[cfg(not(any(target_os = "linux", target_os = "windows")))] { unimplemented!("`perf` is not available for macOS") }
    } else {
        Command::new(&python)
    };
    cmd.arg("OoTRandomizer.py");
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
    }
    cmd.arg("--settings=-");
    cmd.stdin(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.current_dir(repo_path);
    let mut child = cmd.spawn().at_command(cmd_name.clone())?;
    child.stdin.as_mut().expect("configured").write_all(&serde_json::to_vec(&resolved_settings)?).await.at_command(cmd_name.clone())?;
    let output = child.wait_with_output().await.at_command(cmd_name.clone())?;
    let stderr = BufRead::lines(&*output.stderr).try_collect::<_, Vec<_>, _>().at_command(cmd_name)?;
    if output.status.success() {
        if let Some(distribution_file_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Copied distribution file to: ")) {
            fs::remove_file(distribution_file_path).await?;
        }
        if let Some(compressed_rom_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Created compressed ROM at: ")) {
            if cfg!(target_os = "windows") && output_mode == OutputMode::Bench {
                Command::new(WSL).arg("rm").arg(compressed_rom_path).check("wsl rm").await?;
            } else {
                fs::remove_file(compressed_rom_path).await?;
            }
        }
        if let Some(cosmetics_log_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Created cosmetic log at: ")) {
            if cfg!(target_os = "windows") && output_mode == OutputMode::Bench {
                Command::new(WSL).arg("rm").arg(cosmetics_log_path).check("wsl rm").await?;
            } else {
                fs::remove_file(cosmetics_log_path).await?;
            }
        }
    }
    Ok(RollOutput {
        instructions: if let OutputMode::Bench = output_mode {
            if_chain! {
                if let Some(instructions_line) = stderr.iter().rev().find(|line| line.contains("instructions:u"));
                if let Some((_, instructions)) = regex_captures!("^ *([0-9,.]+) +instructions:u", instructions_line);
                then {
                    Ok(instructions.chars().filter(|&c| c != ',' && c != '.').collect::<String>().parse()?)
                } else {
                    Err(output.stderr.clone().into())
                }
            }
        } else {
            Err(Bytes::from_static(b"output mode"))
        },
        patch: if output.status.success() {
            if let Some(patch_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Created patch file archive at: ")) {
                Some((cfg!(target_os = "windows") && output_mode == OutputMode::Bench, PathBuf::from(patch_path)))
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
    })
}

pub async fn run_rsl(repo_path: &Path, bench: bool) -> Result<RollOutput, RollError> {
    let python = python()?;
    #[cfg_attr(not(target_os = "windows"), allow(unused_mut))] let mut cmd_name = python.display().to_string();
    let mut cmd = if bench {
        #[cfg(any(target_os = "linux", target_os = "windows"))] {
            let mut cmd = {
                #[cfg(target_os = "linux")] {
                    Command::new("perf")
                }
                #[cfg(target_os = "windows")] {
                    cmd_name = format!("{WSL} {cmd_name}");
                    let mut cmd = Command::new(WSL);
                    // install using `apt-get install linux-tools-generic` and symlink from `/usr/lib/linux-tools/*-generic/perf`
                    cmd.arg("perf");
                    cmd
                }
            };
            cmd.arg("stat");
            cmd.arg("--event=instructions:u");
            cmd.arg("/usr/bin/python3");
            cmd
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))] { unimplemented!("`perf` is not available for macOS") }
    } else {
        Command::new(&python)
    };
    cmd.arg("RandomSettingsGenerator.py");
    cmd.arg("--no_log_errors");
    cmd.arg("--plando_retries=1");
    cmd.arg("--rando_retries=1");
    cmd.current_dir(repo_path);
    let output = cmd.output().await.at_command(cmd_name.clone())?;
    let stderr = BufRead::lines(&*output.stderr).try_collect::<_, Vec<_>, _>().at_command(cmd_name.clone())?;
    if output.status.success() || output.status.code() == Some(3) {
        let stdout = BufRead::lines(&*output.stdout).try_collect::<_, Vec<_>, _>().at_command(cmd_name)?;
        Ok(RollOutput {
            instructions: if bench {
                let instructions_line = stderr.iter().rev().find(|line| line.contains("instructions:u")).ok_or_else(|| RollError::PerfSyntax(output.stderr.clone()))?;
                let (_, instructions) = regex_captures!("^ *([0-9,.]+) +instructions:u", instructions_line).ok_or_else(|| RollError::PerfSyntax(output.stderr.clone()))?;
                Ok(instructions.chars().filter(|&c| c != ',' && c != '.').collect::<String>().parse()?)
            } else {
                Err(Bytes::from_static(b"output mode"))
            },
            log: if stdout.iter().rev().any(|line| line.starts_with("rsl_tools.RandomizerError")) {
                Err(output.stdout.into())
            } else {
                if let Some(distribution_file_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Copied distribution file to: ")) {
                    fs::remove_file(distribution_file_path).await?;
                }
                if let Some(patch_file_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Creating Patch File: ")) {
                    fs::remove_file(repo_path.join("patches").join(patch_file_path)).await?;
                }
                if let Some(cosmetics_log_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Creating Cosmetics Log: ")) {
                    fs::remove_file(repo_path.join("patches").join(cosmetics_log_path)).await?;
                }
                Ok(repo_path.join("patches").join(stdout.iter().rev().find_map(|line| line.strip_prefix("Created spoiler log at: ")).ok_or_else(|| RollError::SpoilerLogPath(output))?))
            },
            patch: None, //TODO?
        })
    } else {
        Err(RollError::RslScriptExit(output))
    }
}
