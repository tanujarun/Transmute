//! Archive conversion through 7-Zip: unpack everything, then repack.

use crate::tools::{hidden, run, Tools};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

fn unpack(tools: &Tools, archive: &Path, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut target = std::ffi::OsString::from("-o");
    target.push(dir);
    run(hidden(tools.seven_zip()?).args(["x", "-y", "-bso0", "-bsp0"]).arg(target).arg(archive))?;
    Ok(())
}

/// Unpacks an archive into `dir`, following a .tar.gz down to its real contents.
pub fn extract(tools: &Tools, archive: &Path, dir: &Path) -> Result<()> {
    unpack(tools, archive, dir)?;
    if let Some(inner) = only_entry(dir)? {
        if inner.extension().is_some_and(|e| e.eq_ignore_ascii_case("tar")) {
            unpack(tools, &inner, dir)?;
            std::fs::remove_file(inner)?;
        }
    }
    Ok(())
}

fn only_entry(dir: &Path) -> Result<Option<PathBuf>> {
    let entries: Vec<_> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    Ok(if entries.len() == 1 { entries.into_iter().next() } else { None })
}

fn pack(tools: &Tools, kind: &str, contents: &Path, out: &Path) -> Result<()> {
    let mut command = hidden(tools.seven_zip()?);
    command.args(["a", "-y", "-bso0", "-bsp0"]).arg(format!("-t{kind}")).arg(out);
    command.arg(".").current_dir(contents);
    run(&mut command)?;
    Ok(())
}

pub fn convert(tools: &Tools, src: &Path, target: &str, out: &Path, work: &Path) -> Result<()> {
    let contents = work.join("contents");
    unpack(tools, src, &contents)?;
    // A .tar.gz unpacks to a single .tar, which holds the real contents.
    let mut root = contents.clone();
    if let Some(inner) = only_entry(&contents)? {
        if inner.extension().is_some_and(|e| e.eq_ignore_ascii_case("tar")) {
            root = work.join("contents-inner");
            unpack(tools, &inner, &root)?;
        }
    }
    match target {
        "zip" | "7z" | "tar" => pack(tools, target, &root, out),
        "gz" => {
            let tar = work.join("repacked.tar");
            pack(tools, "tar", &root, &tar)?;
            let mut command = hidden(tools.seven_zip()?);
            command.args(["a", "-y", "-bso0", "-bsp0", "-tgzip"]).arg(out).arg(&tar);
            run(&mut command)?;
            Ok(())
        }
        other => bail!("no archive route to {other}"),
    }
}
