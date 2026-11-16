use std::ffi::OsString;

use serde_json::json;

use super::store::Instance;
use super::wsl::{
    delete_script, distro_for, parse_distros, pick_distro, run_in, typesense_start_script, typesense_stop_script,
    wslenv_with, NO_DISTRO,
};
use crate::php::manifest::{Manifest, Os};
use crate::StokePaths;

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn typesense(distro: &str) -> Instance {
    Instance {
        id: "typesense".into(),
        kind: "typesense".into(),
        line: "30".into(),
        port: 8108,
        autostart: true,
        name: "Typesense 30".into(),
        secret: "s3cr3t".into(),
        distro: distro.into(),
    }
}

#[test]
fn distros_are_read_from_utf16_and_utf8() {
    assert_eq!(parse_distros(&utf16("Ubuntu\r\ndocker-desktop\r\n")), names(&["Ubuntu", "docker-desktop"]));
    assert_eq!(parse_distros(&utf16("\u{feff}Debian\r\n\r\n")), names(&["Debian"]));
    assert_eq!(parse_distros(b"Ubuntu-24.04\ndocker-desktop\n"), names(&["Ubuntu-24.04", "docker-desktop"]));
    assert!(parse_distros(b"").is_empty());
}

#[test]
fn docker_desktop_distros_are_skipped() {
    assert_eq!(pick_distro(&names(&["Ubuntu", "Debian"])), Some("Ubuntu".into()), "the default comes first");
    assert_eq!(pick_distro(&names(&["docker-desktop", "docker-desktop-data", "Debian"])), Some("Debian".into()));
    assert_eq!(pick_distro(&names(&["docker-desktop", "docker-desktop-data"])), None);
    assert_eq!(pick_distro(&[]), None);
}

#[test]
fn an_instance_keeps_its_distro() {
    let installed = names(&["Ubuntu", "Debian"]);
    assert_eq!(distro_for(&typesense(""), &installed).unwrap(), "Ubuntu");
    assert_eq!(distro_for(&typesense("Debian"), &installed).unwrap(), "Debian");
    let gone = distro_for(&typesense("Arch"), &installed).unwrap_err().to_string();
    assert_eq!(gone, "Typesense 30 ran in the WSL distro Arch, which is not installed anymore");
    let none = distro_for(&typesense(""), &names(&["docker-desktop"])).unwrap_err().to_string();
    assert_eq!(none, "Typesense 30 needs a WSL distro such as Ubuntu (run: wsl --install -d Ubuntu)");
}

#[test]
fn commands_pass_values_as_arguments_not_as_shell_text() {
    let run = run_in("Ubuntu", "echo \"$1\"", &[OsString::from("a b; rm -rf /")]);
    assert!(run.program.ends_with("wsl.exe"));
    let args: Vec<String> = run.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(args, ["-d", "Ubuntu", "--exec", "/bin/sh", "-c", "echo \"$1\"", "sh", "a b; rm -rf /"]);
}

#[test]
fn scripts_stay_inside_the_home_folder_and_find_their_server() {
    for script in [typesense_start_script(), typesense_stop_script(), delete_script()] {
        assert!(script.starts_with(r#"d="${HOME:?}/.local/share/stoke/services/$1""#), "{script}");
    }
    let start = typesense_start_script();
    assert!(start.contains(r#"echo $$ > "$d/stoke.pid" && exec "$2" --data-dir "$d""#), "{start}");
    assert!(start.contains(r#"--api-address 127.0.0.1 --api-port "$3""#), "{start}");
    assert!(!start.contains("api-key"), "the key goes through the environment");
    // Linux cuts a program name to 15 characters.
    assert!(typesense_stop_script().contains("typesense-serv*)"));
}

#[test]
fn the_key_is_added_to_wslenv_once() {
    assert_eq!(wslenv_with(None, "TYPESENSE_API_KEY"), OsString::from("TYPESENSE_API_KEY/u"));
    assert_eq!(wslenv_with(Some("".into()), "TYPESENSE_API_KEY"), OsString::from("TYPESENSE_API_KEY/u"));
    assert_eq!(wslenv_with(Some("PATH/l".into()), "TYPESENSE_API_KEY"), OsString::from("PATH/l:TYPESENSE_API_KEY/u"));
    assert_eq!(wslenv_with(Some("TYPESENSE_API_KEY".into()), "TYPESENSE_API_KEY"), OsString::from("TYPESENSE_API_KEY"));
}

#[test]
fn typesense_runs_through_wsl_with_its_key_in_the_environment() {
    let d = tempfile::tempdir().unwrap();
    let paths = StokePaths::at(d.path().join("stoke"));
    let spec = super::kinds::wsl_spec(&paths, &typesense("Ubuntu"), "/mnt/c/stoke/typesense-server", None);
    assert!(spec.program.ends_with("wsl.exe"));
    let args: Vec<String> = spec.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(args[..3], ["-d", "Ubuntu", "--exec"]);
    assert_eq!(args[args.len() - 4..], ["typesense", "/mnt/c/stoke/typesense-server", "8108", "8109"]);
    assert!(!args.iter().any(|a| a.contains("s3cr3t")), "the key is not on the command line");
    assert!(spec.env.contains(&("TYPESENSE_API_KEY".into(), "s3cr3t".into())));
    assert!(spec.env.contains(&("WSLENV".into(), "TYPESENSE_API_KEY/u".into())));
    assert_eq!((spec.ready_port, spec.guard_port), (Some(8108), Some(8108)));
}

#[test]
fn a_typesense_stop_goes_through_wsl_on_windows_only() {
    let stop = super::kinds::wsl_stop(&typesense("Ubuntu"));
    assert_eq!(stop.is_some(), cfg!(windows));
    assert!(super::kinds::wsl_stop(&typesense("")).is_none(), "never started: nothing to stop");
}

#[test]
fn instances_saved_before_wsl_support_still_load() {
    let old = json!({ "id": "redis", "kind": "redis", "line": "8", "port": 6379, "autostart": true, "name": "Redis 8", "secret": "" });
    let inst: Instance = serde_json::from_value(old).unwrap();
    assert_eq!(inst.distro, "");
    assert!(serde_json::to_value(&inst).unwrap().get("distro").is_none(), "no empty field written");
}

fn manifest(wsl: serde_json::Value) -> Manifest {
    let mut m = json!({
        "schema": 1, "generated": "2026-11-16T00:00:00Z", "php": [],
        "composer": { "url": "https://x/composer.phar", "sha256": "0".repeat(64) },
        "cacert": { "url": "https://x/cacert.pem", "sha256": "0".repeat(64) },
    });
    if !wsl.is_null() {
        m["wsl"] = wsl;
    }
    serde_json::from_value(m).unwrap()
}

#[test]
fn a_catalog_with_or_without_wsl_builds_parses() {
    assert!(manifest(serde_json::Value::Null).wsl.is_empty(), "catalogs before the wsl list");
    let m = manifest(json!([{ "kind": "typesense", "line": "30", "version": "30.2", "os": "linux", "arch": "x64",
        "url": "https://dl.typesense.org/t.tar.gz", "sha256": "1".repeat(64), "format": "tar.gz", "root": "" }]));
    assert_eq!(m.wsl[0].as_service().os, Os::Windows);
}

#[test]
fn windows_lists_typesense_only_with_a_distro() {
    let m = manifest(json!([{ "kind": "typesense", "line": "30", "version": "30.2", "os": "linux", "arch": "x64",
        "url": "https://dl.typesense.org/t.tar.gz", "sha256": "1".repeat(64), "format": "tar.gz", "root": "" }]));
    let with = super::api::listing(&m, true);
    let without = super::api::listing(&m, false);
    if cfg!(windows) {
        assert_eq!(with["builds"][0]["kind"], "typesense");
        assert_eq!(with["unavailable"], json!([]));
        assert_eq!(without["builds"], json!([]));
        assert_eq!(without["unavailable"][0]["reason"], NO_DISTRO);
        let empty = super::api::listing(&manifest(serde_json::Value::Null), true);
        assert_eq!(empty["unavailable"][0]["reason"], "Not in the catalog for WSL yet");
    } else {
        assert_eq!((with["builds"].clone(), without["unavailable"].clone()), (json!([]), json!([])));
    }
}
