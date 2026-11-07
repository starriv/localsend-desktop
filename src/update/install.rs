//! Replacing the installed app with a downloaded, verified update.
//!
//! - **macOS**: the update is the `.zip` of `LocalSend.app`. It is unpacked
//!   next to the download with `ditto`, its bundle id, version and code
//!   signature are checked, and a small shell script (started detached, see
//!   [`launch`]) waits for this process to exit, swaps the bundles (keeping
//!   the old one until the new one is in place) and, when asked to, opens the
//!   new one.
//! - **Windows**: the update is the NSIS installer. It is started with `/S`
//!   (silent) and `/UPDATE`; the installer waits for `LocalSend.exe` to
//!   exit, installs over the same folder and starts the app again, unless
//!   `/NORUN` is also passed.
//!
//! [`launch`] either relaunches ("Restart to update") or not (the update is
//! installed as the user quits the app).
//!
//! [`target`] decides first whether this copy can update itself at all: a
//! development build, a portable Windows copy, a translocated or read-only
//! macOS bundle can not, and the UI points to the release page instead.

use super::{Error, Result};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use anyhow::Context;
use std::path::{Path, PathBuf};

/// Why this copy has to be updated by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manual {
    /// Not running from an installed app (`cargo run`, an unsupported
    /// platform or architecture).
    Unsupported,
    /// Windows: the portable zip, not the installer.
    Portable,
    /// macOS: started from a quarantined download (App Translocation); the
    /// app has to be moved to Applications first.
    Translocated,
    /// The app's folder is not writable by this user.
    ReadOnly,
}

/// The installed app this process updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// macOS: the `.app` bundle; Windows: the install folder.
    pub path: PathBuf,
}

/// A verified update ready for [`launch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub target: Target,
    /// macOS: the unpacked `.app`; Windows: the installer.
    pub payload: PathBuf,
}

/// The installed app, or why it cannot update itself.
pub fn target() -> std::result::Result<Target, Manual> {
    if super::platform_key().is_none() {
        return Err(Manual::Unsupported);
    }
    let exe = std::env::current_exe().map_err(|_| Manual::Unsupported)?;
    let exe = exe.canonicalize().unwrap_or(exe);
    platform_target(&exe)
}

#[cfg(target_os = "macos")]
fn platform_target(exe: &Path) -> std::result::Result<Target, Manual> {
    let app = app_bundle_of(exe).ok_or(Manual::Unsupported)?;
    if app
        .components()
        .any(|part| part.as_os_str() == "AppTranslocation")
    {
        return Err(Manual::Translocated);
    }
    let parent = app.parent().ok_or(Manual::Unsupported)?;
    if !writable(parent) || !writable(&app.join("Contents")) {
        return Err(Manual::ReadOnly);
    }
    Ok(Target { path: app })
}

#[cfg(target_os = "windows")]
fn platform_target(exe: &Path) -> std::result::Result<Target, Manual> {
    let dir = exe.parent().ok_or(Manual::Unsupported)?;
    // The installer writes its uninstaller next to the app; the portable zip
    // has none, and updating it with the installer would install a second
    // copy elsewhere.
    if !dir.join("uninstall.exe").is_file() {
        return Err(Manual::Portable);
    }
    if !writable(dir) {
        return Err(Manual::ReadOnly);
    }
    Ok(Target {
        path: dir.to_path_buf(),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_target(_exe: &Path) -> std::result::Result<Target, Manual> {
    Err(Manual::Unsupported)
}

/// `…/X.app` for `…/X.app/Contents/MacOS/<binary>`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn app_bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let shape = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";
    shape.then(|| app.to_path_buf())
}

/// Whether a file can be created in `dir` (permissions alone do not say:
/// ACLs, read-only volumes and sandboxing all matter).
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".localsend-update-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Turns the downloaded `file` into something [`launch`] can install.
/// Blocking (unpacking runs `ditto`): call it from `spawn_blocking`.
pub fn prepare(target: &Target, file: &Path, version: &semver::Version) -> Result<Prepared> {
    platform_prepare(target, file, version).map(|payload| Prepared {
        target: target.clone(),
        payload,
    })
}

#[cfg(target_os = "macos")]
fn platform_prepare(target: &Target, file: &Path, version: &semver::Version) -> Result<PathBuf> {
    use std::process::Command;

    let dir = file
        .parent()
        .ok_or_else(|| Error::install(anyhow::anyhow!("the download has no folder")))?;
    let unpacked = dir.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked).map_err(Error::install)?;
    run(Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(file)
        .arg(&unpacked))
    .map_err(Error::install)?;

    let name = target
        .path
        .file_name()
        .ok_or_else(|| Error::install(anyhow::anyhow!("the app has no name")))?;
    let app = unpacked.join(name);
    if !app.join("Contents/Info.plist").is_file() {
        return Err(Error::install(anyhow::anyhow!(
            "the update has no {}",
            Path::new(name).display()
        )));
    }

    let id = |bundle: &Path| plist_value(bundle, "CFBundleIdentifier");
    let installed_id = id(&target.path).map_err(Error::install)?;
    let update_id = id(&app).map_err(Error::install)?;
    if installed_id != update_id {
        return Err(Error::verification(anyhow::anyhow!(
            "the update is {update_id}, the installed app is {installed_id}"
        )));
    }
    let update_version = plist_value(&app, "CFBundleShortVersionString").map_err(Error::install)?;
    if super::parse_version(&update_version).ok().as_ref() != Some(version) {
        return Err(Error::verification(anyhow::anyhow!(
            "the update bundle is version {update_version}, the manifest states {version}"
        )));
    }
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app))
    .map_err(Error::verification)?;
    // Downloads made by this process carry no quarantine flag; strip one
    // anyway so the new version opens without the "downloaded from the
    // internet" prompt. Failing to do so is harmless.
    let _ = Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&app)
        .output();
    Ok(app)
}

#[cfg(target_os = "windows")]
fn platform_prepare(_target: &Target, file: &Path, _version: &semver::Version) -> Result<PathBuf> {
    // The installer is verified by its SHA-256; NSIS checks its own CRC.
    Ok(file.to_path_buf())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_prepare(_target: &Target, _file: &Path, _version: &semver::Version) -> Result<PathBuf> {
    Err(Error::install(anyhow::anyhow!(
        "self-update is not supported here"
    )))
}

#[cfg(target_os = "macos")]
fn plist_value(bundle: &Path, key: &str) -> anyhow::Result<String> {
    let output = std::process::Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(format!("Print :{key}"))
        .arg(bundle.join("Contents/Info.plist"))
        .output()
        .context("cannot run PlistBuddy")?;
    if !output.status.success() {
        return Err(anyhow::anyhow!("{} has no {key}", bundle.display()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(target_os = "macos")]
fn run(command: &mut std::process::Command) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("cannot run {:?}", command.get_program()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{:?} failed ({}): {}",
            command.get_program(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The macOS swap script. Arguments: the pid to wait for, the new bundle, the
/// installed bundle, the log file, and `1` to open the app afterwards (`0`
/// when the user quit it). It never replaces a running app: if the process is
/// still alive after a minute it gives up.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const SWAP_SCRIPT: &str = r#"#!/bin/sh
pid="$1"; new="$2"; target="$3"; log="$4"; relaunch="$5"
reopen() { [ "$relaunch" = 1 ] && open "$target"; }
exec >>"$log" 2>&1
echo "$(date): waiting for $pid"
i=0
while kill -0 "$pid" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 300 ]; then echo "the app did not quit; update abandoned"; exit 1; fi
  sleep 0.2
done
backup="$target.update-backup"
rm -rf "$backup"
if ! mv "$target" "$backup"; then
  echo "cannot move the installed app aside"
  reopen
  exit 1
fi
if mv "$new" "$target"; then
  rm -rf "$backup"
  echo "updated"
else
  echo "cannot move the update in place; restoring"
  rm -rf "$target"
  mv "$backup" "$target"
fi
reopen
exit 0
"#;

/// Starts the detached process that installs `prepared` once this process
/// has exited, and with `relaunch` starts the updated app afterwards. The
/// caller quits right after it returns `Ok`.
pub fn launch(prepared: &Prepared, relaunch: bool) -> Result<()> {
    platform_launch(prepared, relaunch).map_err(Error::install)
}

#[cfg(target_os = "macos")]
fn platform_launch(prepared: &Prepared, relaunch: bool) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let dir = prepared
        .payload
        .parent()
        .and_then(Path::parent)
        .context("the update has no staging folder")?;
    let script = dir.join("swap.sh");
    std::fs::write(&script, SWAP_SCRIPT).context("cannot write the update script")?;
    Command::new("/bin/sh")
        .arg(&script)
        .arg(std::process::id().to_string())
        .arg(&prepared.payload)
        .arg(&prepared.target.path)
        .arg(dir.join("update.log"))
        .arg(if relaunch { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group: nothing sent to the app's group reaches it.
        .process_group(0)
        .spawn()
        .context("cannot start the update script")?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn platform_launch(prepared: &Prepared, relaunch: bool) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let mut command = std::process::Command::new(&prepared.payload);
    command.args(["/S", "/UPDATE"]);
    if !relaunch {
        // Installers older than this flag ignore it and start the app.
        command.arg("/NORUN");
    }
    command
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .context("cannot start the installer")?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_launch(_prepared: &Prepared, _relaunch: bool) -> anyhow::Result<()> {
    Err(anyhow::anyhow!("self-update is not supported here"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_found_from_its_binary() {
        assert_eq!(
            app_bundle_of(Path::new(
                "/Applications/LocalSend.app/Contents/MacOS/LocalSend"
            )),
            Some(PathBuf::from("/Applications/LocalSend.app"))
        );
        assert_eq!(
            app_bundle_of(Path::new("/Users/me/repo/target/debug/localsend-desktop")),
            None
        );
    }

    #[test]
    fn a_scratch_folder_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(writable(dir.path()));
        assert!(!writable(&dir.path().join("missing")));
    }

    /// The swap script against real folders: the old bundle is replaced, and
    /// a missing update leaves the installed one in place.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_swap_script_replaces_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("swap.sh");
        // Never open anything from a test: relaunch is `0` below, and `open`
        // is stubbed out as well.
        std::fs::write(&script, SWAP_SCRIPT.replace("open \"$target\"", ":")).unwrap();
        let target = dir.path().join("App.app");
        let new = dir.path().join("new/App.app");
        std::fs::create_dir_all(target.join("Contents")).unwrap();
        std::fs::write(target.join("Contents/v"), "old").unwrap();
        std::fs::create_dir_all(new.join("Contents")).unwrap();
        std::fs::write(new.join("Contents/v"), "new").unwrap();
        let log = dir.path().join("log");
        let run = |new: &Path| {
            std::process::Command::new("/bin/sh")
                .arg(&script)
                // A pid that is not running.
                .arg("999999")
                .arg(new)
                .arg(&target)
                .arg(&log)
                .arg("0")
                .status()
                .unwrap()
        };
        assert!(run(&new).success());
        assert_eq!(
            std::fs::read_to_string(target.join("Contents/v")).unwrap(),
            "new"
        );
        assert!(!dir.path().join("App.app.update-backup").exists());

        run(&dir.path().join("missing.app"));
        assert_eq!(
            std::fs::read_to_string(target.join("Contents/v")).unwrap(),
            "new"
        );
    }
}
