mod args;
mod errors;

use crate::args::{Args, SubCommand};
use crate::errors::*;
use env_logger::Env;
use filetime::FileTime;
use libflate::gzip::Decoder;
use std::env;
use std::fs;
use std::fs::File;
use std::fs::Permissions;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use structopt::StructOpt;
use tar::Archive;
use tempfile::tempdir;

fn read_image(path: &Path, arch: &str) -> Result<(PathBuf, Vec<u8>)> {
    let f = File::open(path).context("Failed to open image")?;
    let d = Decoder::new(f).context("Failed to open image as gzip")?;
    let mut a = Archive::new(d);
    let entries = a.entries().context("Failed to read image as archive")?;

    let needle = format!("./apks/{}/APKINDEX.tar.gz", arch);

    info!("Searching for APKINDEX.tar.gz in image");
    for entry in entries {
        let entry = entry.context("Failed to read entry")?;
        let path = entry.path().context("Failed to get path from entry")?;

        debug!("Reading entry in image: {:?}", path);

        if path == Path::new(&needle) {
            info!("Found index: {:?}", path);
            return read_index(entry);
        }
    }

    bail!("Index not found in image");
}

fn read_index<R: Read>(r: R) -> Result<(PathBuf, Vec<u8>)> {
    let d = Decoder::new(r).context("Failed to open index as gzip")?;

    let mut a = Archive::new(d);
    let entries = a.entries().context("Failed to read image as archive")?;

    info!("Searching for signature in APKINDEX.tar.gz");

    for entry in entries {
        let mut entry = entry.context("Failed to read entry")?;
        let path = {
            let path = entry.path().context("Failed to get path from entry")?;
            path.to_path_buf()
        };

        debug!("Reading entry in index: {:?}", path);

        if path.to_str().map_or(false, |x| x.starts_with(".SIGN.")) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok((path, buf));
        }
    }

    bail!("No signature found in APKINDEX.tar.gz")
}

fn read_signature(subcommand: &SubCommand) -> Result<(PathBuf, Vec<u8>)> {
    let (name, sig) = match subcommand {
        SubCommand::FromImage { path, arch } => read_image(&path, &arch)?,
        SubCommand::FromIndex { path } => {
            let f = File::open(path).context("Failed to open image")?;
            read_index(f)?
        }
        SubCommand::FromFile { path } => {
            let name = path.file_name().context("Failed to determine filename")?;
            let name = PathBuf::from(name);
            let sig = fs::read(path)?;
            (name, sig)
        }
    };

    info!("Found signature {:?}", name);
    debug!("Signature content: {:?}", sig);

    Ok((name, sig))
}

fn get_source_date_epoch() -> Option<i64> {
    let sde = env::var("SOURCE_DATE_EPOCH").ok()?;
    sde.parse().ok()
}

fn wait_check_exit(mut c: Child, name: &str) -> Result<()> {
    let exit = c
        .wait()
        .with_context(|| anyhow!("Failed to wait for child: {:?}", name))?;
    check_exit(exit, name)
}

fn check_exit(exit: ExitStatus, name: &str) -> Result<()> {
    debug!("Child exited with status {:?}, {:?}", name, exit);
    if !exit.success() {
        bail!("Command failed: {:?}, {:?}", name, exit);
    }
    Ok(())
}

fn sign_archive(index_path: &Path, output_path: &Path, sig_name: &Path, sig: &[u8]) -> Result<()> {
    /*
    tar -f - -c "$sig" | abuild-tar --cut | $gzip -n -9 > "$tmptargz"
    tmpsigned=$(mktemp)
    cat "$tmptargz" "$i" > "$tmpsigned"
    rm -f "$tmptargz" "$sig"
    chmod 644 "$tmpsigned"
    mv "$tmpsigned" "$i"
    msg "Signed $i"
    */

    let dir = tempdir()?;
    debug!("Created temporary directory: {:?}", dir.path());

    let sig_path = dir.path().join(sig_name);
    debug!("Writing signature to file: {:?}", sig_path);
    fs::write(&sig_path, sig)?;

    if let Some(timestamp) = get_source_date_epoch() {
        let mtime = FileTime::from_unix_time(timestamp, 0);
        debug!("Changing mtime of {:?} to {}", sig_path, mtime);
        filetime::set_file_mtime(&sig_path, mtime)?;
    }

    info!("Creating signed index with existing signature");
    let mut tar_cmd = Command::new("tar")
        .arg("--owner=0")
        .arg("--group=0")
        .arg("--numeric-owner")
        .arg("-C")
        .arg(dir.path())
        .arg("-f")
        .arg("-")
        .arg("-c")
        .arg(sig_name)
        .stdout(Stdio::piped())
        .spawn()
        .context("Failed to spawn tar")?;

    let tar_stdout = tar_cmd.stdout.take().unwrap();

    let mut abuild_cmd = Command::new("abuild-tar")
        .arg("--cut")
        .stdin(Stdio::from(tar_stdout))
        .stdout(Stdio::piped())
        .spawn()
        .context("Failed to spawn abuild-tar")?;

    let abuild_stdout = abuild_cmd.stdout.take().unwrap();

    let gzip_out = Command::new("gzip")
        .args(&["-n", "-9"])
        .stdin(Stdio::from(abuild_stdout))
        .stdout(Stdio::piped())
        .output()
        .context("Failed to run gzip")?;

    check_exit(gzip_out.status, "gzip")?;
    wait_check_exit(abuild_cmd, "abuild-tar")?;
    wait_check_exit(tar_cmd, "tar")?;

    let mut signed_index = gzip_out.stdout;

    info!("Appending package index: {:?}", index_path);
    let mut f = File::open(index_path)
        .with_context(|| anyhow!("Failed to open index at {:?}", index_path))?;
    f.read_to_end(&mut signed_index)
        .context("Failed to read index")?;

    info!("Writing signed index: {:?}", output_path);
    fs::write(&output_path, &signed_index)
        .with_context(|| anyhow!("Failed to write signed index to {:?}", output_path))?;

    debug!("Changing mode to 644");
    let perms = Permissions::from_mode(0o644);
    fs::set_permissions(output_path, perms).context("Failed to chmod file")?;

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::from_args();

    let logging = match (args.quiet, args.verbose) {
        (true, _) => "warn",
        (false, 0) => "info",
        (false, 1) => "info,abuild_reusesig=debug",
        (false, 2) => "debug",
        (false, _) => "debug,abuild_reusesig=trace",
    };
    env_logger::init_from_env(Env::default().default_filter_or(logging));

    let (name, sig) = read_signature(&args.subcommand)?;
    sign_archive(&args.index_path, &args.output_path, &name, &sig)?;

    Ok(())
}
