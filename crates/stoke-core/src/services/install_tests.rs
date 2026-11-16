use super::install::{available, ensure_installed};
use crate::php::manifest::Os;
use crate::php::service_tests::fixture;

#[tokio::test(flavor = "multi_thread")]
async fn a_service_is_installed_once_and_reused_without_the_catalog() {
    let f = fixture(false);
    let first = ensure_installed(&f.core, "redis", "8").await.unwrap();
    assert!(first.starts_with(f.core.paths.data.join("services").join("bin").join("redis").join("8.10.3")));
    let server = if cfg!(windows) { "redis-server.exe" } else { "bin/redis-server" };
    assert!(first.join(server).is_file(), "the archive's top folder is stripped: {}", first.display());
    let second = ensure_installed(&f.core, "redis", "8").await.unwrap();
    assert_eq!(first, second);
    let hits = f.hits.lock().unwrap();
    assert_eq!(hits.get("/svc/redis").copied(), Some(1), "downloaded once");
    assert_eq!(hits.get("/manifest.json").copied(), Some(1), "no catalog request once installed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_program_is_installed_under_its_kind_name() {
    let f = fixture(false);
    let dir = ensure_installed(&f.core, "meilisearch", "1").await.unwrap();
    let program = dir.join(if cfg!(windows) { "meilisearch.exe" } else { "meilisearch" });
    assert_eq!(std::fs::read(&program).unwrap(), b"meilisearch program");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_download_leaves_nothing_installed() {
    let f = fixture(false);
    let err = ensure_installed(&f.core, "postgresql", "18").await.unwrap_err();
    assert!(err.to_string().contains("damaged") || err.to_string().contains("does not match"), "{err}");
    let versions = f.core.paths.data.join("services").join("bin").join("postgresql");
    let left = std::fs::read_dir(&versions)
        .map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
        .unwrap_or(0);
    assert_eq!(left, 0, "no half-installed PostgreSQL");
}

#[tokio::test(flavor = "multi_thread")]
async fn windows_installs_typesense_from_the_wsl_list() {
    let f = fixture(false);
    if Os::current() != Os::Windows {
        return;
    }
    let dir = ensure_installed(&f.core, "typesense", "30").await.unwrap();
    assert!(dir.ends_with(std::path::Path::new("typesense").join("30.2")), "{}", dir.display());
    // The Linux program keeps its name: no ".exe".
    assert_eq!(std::fs::read(dir.join("typesense-server")).unwrap(), b"linux typesense");
    let err = ensure_installed(&f.core, "typesense", "29").await.unwrap_err().to_string();
    assert_eq!(err, "The catalog has no Typesense 29 for WSL yet", "another architecture's build is not used");
}

#[tokio::test(flavor = "multi_thread")]
async fn kinds_missing_for_this_computer_say_why() {
    let f = fixture(false);
    let err = ensure_installed(&f.core, "mysql", "8.4").await.unwrap_err().to_string();
    assert_eq!(err, "The catalog has no MySQL 8.4 for this computer");
}

#[tokio::test(flavor = "multi_thread")]
async fn available_lists_what_this_computer_can_run() {
    let f = fixture(false);
    let manifest =
        crate::php::manifest::fetch(&f.core.paths, &f.core.catalog.url, &f.core.catalog.public_key).await.unwrap();
    let list: Vec<(String, String)> =
        available(&manifest).into_iter().map(|b| (b.kind.clone(), b.line.clone())).collect();
    assert!(list.contains(&("redis".into(), "8".into())));
    assert!(list.contains(&("meilisearch".into(), "1".into())));
    // macOS arm64 from `services`, Windows from `wsl`; an Intel Mac has none in the fixture.
    let typesense = Os::current() == Os::Windows || cfg!(target_arch = "aarch64");
    assert_eq!(list.iter().any(|(k, _)| k == "typesense"), typesense);
    assert!(!list.contains(&("typesense".into(), "29".into())), "only this architecture's WSL build");
}
