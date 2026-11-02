//! Finds the bundled command-line tools and runs them without a console window.

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Locations of the portable tools in the `tools` folder.
#[derive(Clone, Debug)]
pub struct Tools {
    root: PathBuf,
}

impl Tools {
    /// Uses `TRANSMUTE_TOOLS`, or the nearest `tools` folder above the executable.
    pub fn locate() -> Result<Self> {
        if let Some(root) = std::env::var_os("TRANSMUTE_TOOLS") {
            return Ok(Self { root: root.into() });
        }
        let exe = std::env::current_exe()?;
        exe.ancestors()
            .map(|dir| dir.join("tools"))
            .find(|dir| dir.is_dir())
            .map(|root| Self { root })
            .ok_or_else(|| anyhow!("no tools folder found above {}", exe.display()))
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn find(&self, folder: &str, exe: &str) -> Result<PathBuf> {
        find_file(&self.root.join(folder), exe)
            .ok_or_else(|| anyhow!("{exe} is missing; run scripts\\fetch-tools.ps1"))
    }

    pub fn ffmpeg(&self) -> Result<PathBuf> {
        self.find("ffmpeg", "ffmpeg.exe")
    }
    pub fn ffprobe(&self) -> Result<PathBuf> {
        self.find("ffmpeg", "ffprobe.exe")
    }
    pub fn magick(&self) -> Result<PathBuf> {
        self.find("magick", "magick.exe")
    }
    pub fn pandoc(&self) -> Result<PathBuf> {
        self.find("pandoc", "pandoc.exe")
    }
    pub fn seven_zip(&self) -> Result<PathBuf> {
        self.find("7zip", "7z.exe")
    }
    pub fn poppler(&self, exe: &str) -> Result<PathBuf> {
        self.find("poppler", exe)
    }
    pub fn qpdf(&self) -> Result<PathBuf> {
        self.find("qpdf", "qpdf.exe")
    }
    pub fn soffice(&self) -> Result<PathBuf> {
        self.find("libreoffice", "soffice.com").or_else(|_| self.find("libreoffice", "soffice.exe"))
    }
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .find_map(|entry| find_file(&entry.path(), name))
}

pub fn hidden(exe: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(exe);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    let start = trimmed.char_indices().rev().nth(600).map_or(0, |(index, _)| index);
    trimmed[start..].to_string()
}

/// Runs a command to completion, failing with its error output.
pub fn run(command: &mut Command) -> Result<Vec<u8>> {
    let name = command.get_program().to_string_lossy().into_owned();
    let output = command.output().with_context(|| format!("could not start {name}"))?;
    if !output.status.success() {
        let details = if output.stderr.is_empty() { &output.stdout } else { &output.stderr };
        bail!("{} failed: {}", Path::new(&name).display(), tail(details));
    }
    Ok(output.stdout)
}

/// Like `run`, but kills the process if it outlives `limit`.
pub fn run_with_timeout(command: &mut Command, limit: Duration) -> Result<()> {
    let name = command.get_program().to_string_lossy().into_owned();
    let mut child: Child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not start {name}"))?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let output = child.wait_with_output()?;
            bail!("{name} failed: {}", tail(&output.stderr));
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{name} timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
