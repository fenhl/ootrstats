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
    collect_mac::collect,
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
#[cfg(windows)] use directories::UserDirs;

/// install using `wsl --update --pre-release` to get support for the CPU instruction counter and SSH access
const WSL: &str = "C:\\Program Files\\WSL\\wsl.exe";

#[derive(Clone)]
pub enum RandoSettings {
    Default,
    Preset(String),
}

impl RandoSettings {
    pub fn stats_dir(&self) -> Cow<'static, Path> {
        match self {
            Self::Default => Path::new("default").into(),
            Self::Preset(preset) => Path::new("preset").join(preset).into(),
        }
    }
}

pub struct RollOutput {
    /// present iff the `bench` parameter was set.
    pub instructions: Option<u64>,
    /// `Ok`: spoiler log, `Err`: stderr
    pub log: Result<PathBuf, Vec<u8>>,
}

#[derive(Debug, thiserror::Error)]
pub enum RollError {
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] ParseInt(#[from] std::num::ParseIntError),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
    #[error("failed to parse `perf` output")]
    PerfSyntax(Vec<u8>),
    #[error("randomizer did not report spoiler log location")]
    SpoilerLogPath,
}

fn python() -> Result<PathBuf, RollError> {
    Ok({
        #[cfg(windows)] { UserDirs::new().ok_or(RollError::MissingHomeDir)?.home_dir().join("scoop").join("apps").join("python").join("current").join("py.exe") }
        #[cfg(target_os = "linux")] { PathBuf::from("/usr/bin/python3") }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))] { PathBuf::from("/opt/homebrew/bin/python3") }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))] { PathBuf::from("/usr/local/bin/python3") }
    })
}

pub async fn run_rando(base_rom_path: &Path, repo_path: &Path, settings: &RandoSettings, bench: bool) -> Result<RollOutput, RollError> {
    let resolved_settings = collect![as HashMap<_, _>:
        Cow::Borrowed("rom") => json!(base_rom_path),
        Cow::Borrowed("create_spoiler") => json!(true),
        Cow::Borrowed("create_cosmetics_log") => json!(bench),
        Cow::Borrowed("create_compressed_rom") => json!(bench),
    ];
    let python = python()?;
    let mut cmd = if bench {
        #[cfg(any(target_os = "linux", target_os = "windows"))] {
            let mut cmd = {
                #[cfg(target_os = "linux")] {
                    Command::new("perf")
                }
                #[cfg(target_os = "windows")] {
                    let mut cmd = Command::new(WSL);
                    // install using `apt-get install linux-tools-generic` and symlink from `/usr/lib/linux-tools/*-generic/perf`
                    cmd.arg("perf");
                    cmd
                }
            };
            cmd.arg("stat");
            cmd.arg("/usr/bin/python3");
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
        RandoSettings::Preset(preset) => { cmd.arg(format!("--settings_preset={preset}")); }
    }
    cmd.arg("--settings=-");
    cmd.stdin(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.current_dir(repo_path);
    let mut child = cmd.spawn().at_command(python.display().to_string())?;
    child.stdin.as_mut().expect("configured").write_all(&serde_json::to_vec(&resolved_settings)?).await.at_command(python.display().to_string())?;
    let output = child.wait_with_output().await.at_command(python.display().to_string())?;
    let stderr = BufRead::lines(&*output.stderr).try_collect::<_, Vec<_>, _>().at_command(python.display().to_string())?;
    Ok(RollOutput {
        instructions: if bench {
            let instructions_line = stderr.iter().rev().find(|line| line.contains("instructions:u")).ok_or_else(|| RollError::PerfSyntax(output.stderr.clone()))?;
            let (_, instructions) = regex_captures!("^ *([0-9,]+) +instructions:u", instructions_line).ok_or_else(|| RollError::PerfSyntax(output.stderr.clone()))?;
            Some(instructions.chars().filter(|&c| c != ',').collect::<String>().parse()?)
        } else {
            None
        },
        log: if output.status.success() {
            if let Some(distribution_file_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Copied distribution file to: ")) {
                fs::remove_file(distribution_file_path).await?;
            }
            if let Some(compressed_rom_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Created compressed ROM at: ")) {
                if cfg!(target_os = "windows") && bench {
                    Command::new(WSL).arg("rm").arg(compressed_rom_path).check("wsl rm").await?;
                } else {
                    fs::remove_file(compressed_rom_path).await?;
                }
            }
            if let Some(cosmetics_log_path) = stderr.iter().rev().find_map(|line| line.strip_prefix("Created cosmetic log at: ")) {
                if cfg!(target_os = "windows") && bench {
                    Command::new(WSL).arg("rm").arg(cosmetics_log_path).check("wsl rm").await?;
                } else {
                    fs::remove_file(cosmetics_log_path).await?;
                }
            }
            Ok(repo_path.join("Output").join(stderr.iter().rev().find_map(|line| line.strip_prefix("Created spoiler log at: ")).ok_or(RollError::SpoilerLogPath)?))
        } else {
            Err(output.stderr)
        },
    })
}