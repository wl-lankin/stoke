//! Services that run inside WSL on Windows: Typesense ships no Windows build, so its Linux build
//! runs in a WSL distro. The program stays in Stoke's install folder (reached through `/mnt/...`);
//! the data lives inside the distro (`~/.local/share/stoke/services/<id>`), because RocksDB on a
//! Windows drive is slow and fragile. Every command goes through `wsl.exe -d <distro> --exec sh`,
//! with values as positional arguments so nothing needs shell quoting.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::services::kinds::Run;
use crate::services::store::Instance;
use crate::CoreError;

/// Shown when no distro can run a service.
pub const NO_DISTRO: &str = "Needs a WSL distro such as Ubuntu (run: wsl --install -d Ubuntu)";

/// Kinds that run inside WSL on this computer.
pub fn runs_in_wsl(kind: &str) -> bool {
    cfg!(windows) && kind == "typesense"
}

/// `wsl.exe` in System32 (a PATH lookup could find another program of that name).
pub fn exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join("wsl.exe")
}

/// Distro names from `wsl.exe -l -q`, which writes UTF-16 (UTF-8 when `WSL_UTF8=1` took effect).
pub fn parse_distros(bytes: &[u8]) -> Vec<String> {
    let utf16 = bytes.starts_with(&[0xff, 0xfe])
        || (bytes.len() >= 2 && bytes.len().is_multiple_of(2) && bytes.iter().skip(1).step_by(2).all(|b| *b == 0));
    let text = if utf16 {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The distro Stoke uses: the first listed (`wsl -l -q` lists the default first) that is not
/// Docker Desktop's own (`docker-desktop`, `docker-desktop-data`: minimal, reset by Docker).
pub fn pick_distro(list: &[String]) -> Option<String> {
    list.iter().find(|d| !d.to_lowercase().starts_with("docker-desktop")).cloned()
}

/// The installed distros; empty without WSL.
pub fn distros() -> Vec<String> {
    let mut command = std::process::Command::new(exe());
    command.args(["-l", "-q"]).env("WSL_UTF8", "1");
    hide(&mut command);
    match command.output() {
        Ok(out) if out.status.success() => parse_distros(&out.stdout),
        _ => vec![],
    }
}

fn hide(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// The distro an instance runs in: the one it ran in before, else the one Stoke picks now.
pub fn distro_for(inst: &Instance, installed: &[String]) -> Result<String, CoreError> {
    if !inst.distro.is_empty() {
        if installed.iter().any(|d| d == &inst.distro) {
            return Ok(inst.distro.clone());
        }
        return Err(CoreError::Php(format!(
            "{} ran in the WSL distro {}, which is not installed anymore",
            inst.name, inst.distro
        )));
    }
    pick_distro(installed).ok_or_else(|| CoreError::Php(format!("{} {}", inst.name, lower_first(NO_DISTRO))))
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_lowercase().chain(c).collect()).unwrap_or_default()
}

/// `script` run by `sh` in `distro`, with `args` as `$1`, `$2`, ...
pub fn run_in(distro: &str, script: &str, args: &[OsString]) -> Run {
    let mut all: Vec<OsString> =
        ["-d", distro, "--exec", "/bin/sh", "-c", script, "sh"].iter().map(OsString::from).collect();
    all.extend(args.iter().cloned());
    Run { program: exe(), args: all }
}

/// The instance's data folder inside the distro; `$1` is the instance id. `${HOME:?}` stops the
/// script when HOME is unset instead of working below `/`.
const DATA: &str = r#"d="${HOME:?}/.local/share/stoke/services/$1""#;

/// Starts Typesense in the foreground. `exec` keeps the pid written to the file, so a stop (or a
/// start after Stoke was killed) finds the server. Args: id, program, API port, peering port.
pub fn typesense_start_script() -> String {
    format!(
        r#"{DATA}; mkdir -p "$d" && echo $$ > "$d/stoke.pid" && exec "$2" --data-dir "$d" --api-address 127.0.0.1 --api-port "$3" --peering-address 127.0.0.1 --peering-port "$4" --enable-cors"#
    )
}

/// Stops the server named in the pid file, if it still runs (Linux cuts `comm` to 15 characters:
/// `typesense-serve`), waits up to 15 s, then kills it. Arg: id.
pub fn typesense_stop_script() -> String {
    format!(
        r#"{DATA}; p=$(cat "$d/stoke.pid" 2>/dev/null) || exit 0; case "$(cat /proc/$p/comm 2>/dev/null)" in typesense-serv*) ;; *) exit 0 ;; esac; kill -TERM "$p"; i=0; while kill -0 "$p" 2>/dev/null && [ $i -lt 75 ]; do sleep 0.2; i=$((i+1)); done; kill -KILL "$p" 2>/dev/null; exit 0"#
    )
}

/// Removes the instance's data folder. Arg: id.
pub fn delete_script() -> String {
    format!(r#"{DATA}; rm -rf "$d""#)
}

/// `path` as the distro sees it (`/mnt/c/...`, or wherever its automount root is).
pub fn linux_path(distro: &str, path: &Path) -> Result<String, CoreError> {
    let mut command = std::process::Command::new(exe());
    command.args(["-d", distro, "--exec", "wslpath", "-a"]).arg(path);
    hide(&mut command);
    let out = command.output().map_err(|e| CoreError::io(exe(), e))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || text.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(CoreError::Php(format!("WSL ({distro}) cannot reach {}: {err}", path.display())));
    }
    Ok(text)
}

/// `WSLENV` that also hands `name` from Windows to Linux (`/u`: only in that direction).
pub fn wslenv_with(current: Option<OsString>, name: &str) -> OsString {
    let entry = format!("{name}/u");
    match current.map(|c| c.to_string_lossy().into_owned()).filter(|c| !c.is_empty()) {
        Some(c) if c.split(':').any(|e| e.split('/').next() == Some(name)) => c.into(),
        Some(c) => format!("{c}:{entry}").into(),
        None => entry.into(),
    }
}
