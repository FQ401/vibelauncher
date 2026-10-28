//! Loader profiles from synthetic upstream responses: normalization, merge with the vanilla
//! version JSON, version lists, and the cache.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use vl_core::component::ComponentKind::{Fabric, Forge, Quilt};
use vl_core::error::{Error, ErrorKind};
use vl_core::platform::{Arch, Os, Platform};
use vl_meta::libraries::plan;
use vl_meta::loader::{
    CachedProfile, FetchOutcome, LoaderProfile, LoaderVersions, ProfileSource, choose_source,
    profile_cache_key, resolve_profile,
};
use vl_meta::rules::{Features, RuleContext};
use vl_meta::version::VersionJson;

fn read(dir: &str, name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{dir}/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(path).unwrap()
}

fn loader_fixture(name: &str) -> Vec<u8> {
    read("loader", name)
}

fn vanilla(name: &str) -> VersionJson {
    VersionJson::parse(&read("version", name)).unwrap()
}

fn fabric() -> LoaderProfile {
    LoaderProfile::normalize(
        Fabric,
        "1.21.11",
        "0.20.1",
        &loader_fixture("fabric_profile.json"),
    )
    .unwrap()
}

#[test]
fn fabric_and_quilt_normalize_to_one_format() {
    let f = fabric();
    let q = LoaderProfile::normalize(
        Quilt,
        "1.21.11",
        "0.31.0-beta.4",
        &loader_fixture("quilt_profile.json"),
    )
    .unwrap();
    for p in [&f, &q] {
        assert_eq!(p.format, 1);
        assert_eq!(p.inherits_from, "1.21.11");
        assert!(!p.replaces_game_arguments);
        p.validate().unwrap();
    }
    assert_eq!(
        f.main_class,
        "net.fabricmc.loader.impl.launch.knot.KnotClient"
    );
    assert_eq!(
        q.main_class,
        "org.quiltmc.loader.impl.launch.knot.KnotClient"
    );
    assert_eq!(f.component().version, "0.20.1");
    assert_eq!(q.arguments.game.len(), 1);

    // Top-level hashes become `downloads.artifact`.
    let lib = &f.libraries[0];
    assert!(!lib.rest.contains_key("sha1"));
    let a = &lib.rest["downloads"]["artifact"];
    assert_eq!(
        a["path"],
        "net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar"
    );
    assert_eq!(
        a["url"],
        "https://maven.example.invalid/net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar"
    );
    assert_eq!(a["sha1"], "1111111111111111111111111111111111111111");
    assert_eq!(a["size"], 1000);
    // A repository without a trailing slash works too; a library without a hash stays as is.
    let asm = &f.libraries[1].rest["downloads"]["artifact"];
    assert_eq!(
        asm["url"],
        "https://maven.example.invalid/org/ow2/asm/asm/9.9/asm-9.9.jar"
    );
    assert!(!f.libraries[2].rest.contains_key("downloads"));
}

#[test]
fn normalized_libraries_plan_with_hashes() {
    let merged = fabric().merge(&vanilla("modern.json")).unwrap();
    let features = Features::new();
    let ctx = RuleContext::new(
        Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
        },
        &features,
    );
    let p = plan(&merged.libraries, &ctx).unwrap();
    let loader_jar = p
        .downloads
        .iter()
        .find(|f| f.path.ends_with("fabric-loader-0.20.1.jar"))
        .unwrap();
    assert_eq!(
        loader_jar.sha1.as_deref(),
        Some("1111111111111111111111111111111111111111")
    );
    assert_eq!(loader_jar.size, Some(1000));
}

#[test]
fn merge_with_vanilla() {
    let merged = fabric().merge(&vanilla("modern.json")).unwrap();
    assert_eq!(merged.id, "fabric-loader-0.20.1-1.21.11");
    assert_eq!(merged.chain, ["fabric-loader-0.20.1-1.21.11", "1.21.11"]);
    assert_eq!(
        merged.main_class,
        "net.fabricmc.loader.impl.launch.knot.KnotClient"
    );
    assert_eq!(merged.client_jar, "1.21.11");
    assert_eq!(merged.java_version.as_ref().unwrap().major_version, 21);
    // Loader libraries first, vanilla's after.
    assert_eq!(
        merged.libraries[0].name,
        "net.fabricmc:fabric-loader:0.20.1"
    );
    assert!(merged.libraries.len() > 3);
    assert_eq!(
        merged.arguments.jvm.last().unwrap().values,
        ["-DFabricMcEmu= net.minecraft.client.main.Main "]
    );
}

#[test]
fn merge_rejects_the_wrong_game() {
    let e = fabric().merge(&vanilla("legacy_args.json")).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Integrity);
}

#[test]
fn profile_for_another_game_version_is_damaged() {
    let e = LoaderProfile::normalize(
        Fabric,
        "1.20.1",
        "0.20.1",
        &loader_fixture("fabric_profile.json"),
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Integrity);
    assert!(e.message.contains("1.21.11"));
}

#[test]
fn legacy_profile_replaces_game_arguments() {
    let p = LoaderProfile::normalize(
        Forge,
        "1.12.2",
        "14.23.5.2860",
        &loader_fixture("legacy_forge_profile.json"),
    )
    .unwrap();
    assert!(p.replaces_game_arguments);
    assert!(p.arguments.jvm.is_empty());
    let merged = p.merge(&vanilla("legacy_args.json")).unwrap();
    assert_eq!(merged.main_class, "net.minecraft.launchwrapper.Launch");
    let game: Vec<&str> = merged
        .arguments
        .game
        .iter()
        .flat_map(|a| a.values.iter().map(String::as_str))
        .collect();
    assert_eq!(game.last(), Some(&"example.FMLTweaker"));
    assert!(
        !game.contains(&"--gameDir"),
        "the profile's arguments replace the game's"
    );
    // Same-named library: the profile's wins, the vanilla one is not duplicated.
    assert_eq!(
        merged
            .libraries
            .iter()
            .filter(|l| l.key() == "com.example:old-lib")
            .count(),
        1
    );
    assert_eq!(merged.libraries[1].name, "com.example:old-lib:3.0");
}

fn damaged(json: &str) -> Error {
    LoaderProfile::normalize(Fabric, "1.21.11", "0.20.1", json.as_bytes()).unwrap_err()
}

#[test]
fn hostile_documents_are_integrity_errors() {
    let base = |libs: &str, extra: &str| {
        format!(
            r#"{{"id":"p","inheritsFrom":"1.21.11","mainClass":"a.B"{extra},"libraries":[{libs}]}}"#
        )
    };
    let lib = |name: &str, sha1: &str| {
        format!(
            r#"{{"name":"{name}","url":"https://m.example.invalid/","sha1":"{sha1}","size":1}}"#
        )
    };
    let good = "1".repeat(40);
    let cases = [
        "not json".to_owned(),
        r#"{"id":"p","mainClass":"a.B"}"#.to_owned(), // no inheritsFrom
        r#"{"id":"p","inheritsFrom":"1.21.11"}"#.to_owned(), // no mainClass
        r#"{"id":"","inheritsFrom":"1.21.11","mainClass":"a.B"}"#.to_owned(),
        base(&lib("a:b:1", "zz"), ""),
        base(&lib("a:b:1", &"1".repeat(39)), ""),
        base(
            &format!("{},{}", lib("a:b:1", &good), lib("a:b:2", &good)),
            "",
        ), // duplicate
        base(&lib("../evil:b:1", &good), ""),
        base(&lib("nocolon", &good), ""),
        base(
            r#"{"name":"a:b:1","url":"http://insecure.example.invalid/","sha1":"1111111111111111111111111111111111111111","size":1}"#,
            "",
        ),
        base(
            r#"{"name":"a:b:1","downloads":{"artifact":{"path":"p","url":"https://x.invalid/p","sha1":"bad","size":1}}}"#,
            "",
        ),
    ];
    for c in cases {
        assert_eq!(damaged(&c).kind, ErrorKind::Integrity, "{c}");
    }
    // Unknown fields and a hash-less library are fine.
    let ok = base(
        r#"{"name":"a:b:1","future":[1,2]}"#,
        r#","somethingNew":{"x":1}"#,
    );
    LoaderProfile::normalize(Fabric, "1.21.11", "0.20.1", ok.as_bytes()).unwrap();
}

#[test]
fn version_lists() {
    let f = LoaderVersions::parse_array(Fabric, "1.21.11", &loader_fixture("fabric_versions.json"))
        .unwrap();
    assert_eq!(f.versions.len(), 3);
    assert_eq!(f.filtered(true).len(), 2);
    assert_eq!(f.filtered(false).len(), 3);
    assert_eq!(f.recommended().unwrap().version, "0.20.1");
    assert!(f.check("0.20.0").unwrap().stable);
    assert_eq!(f.check("9.9").unwrap_err().kind, ErrorKind::NotFound);

    // Quilt states no `stable` flag: pre-release suffixes mark beta.
    let q = LoaderVersions::parse_array(Quilt, "1.21.11", &loader_fixture("quilt_versions.json"))
        .unwrap();
    assert!(!q.versions[0].stable);
    assert!(q.versions[1].stable);
    assert_eq!(q.recommended().unwrap().version, "0.30.0");

    // Only betas: still recommends the newest.
    let b = LoaderVersions::new(
        Quilt,
        "1.21.11",
        vec![vl_meta::loader::LoaderVersion {
            version: "1.0-beta".into(),
            stable: false,
            ..Default::default()
        }],
    )
    .unwrap();
    assert_eq!(b.recommended().unwrap().version, "1.0-beta");
}

#[test]
fn unsupported_game_version_and_bad_lists() {
    let none = LoaderVersions::parse_array(Quilt, "1.7.10", b"[]").unwrap();
    let e = none.check("0.30.0").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert!(e.message.contains("Quilt") && e.message.contains("1.7.10"));
    assert!(none.recommended().is_none());

    let dup = LoaderVersions::parse_array(Fabric, "1", &loader_fixture("duplicate_versions.json"));
    assert_eq!(dup.unwrap_err().kind, ErrorKind::Integrity);
    for bad in [&b"{}"[..], b"[{\"stable\":true}]", b"[{\"version\":\"\"}]"] {
        assert_eq!(
            LoaderVersions::parse_array(Fabric, "1", bad)
                .unwrap_err()
                .kind,
            ErrorKind::Integrity
        );
    }
}

#[test]
fn cache_round_trip_and_damage() {
    let c = CachedProfile::new(fabric(), Some("\"abc\"".into()), 1_000);
    let bytes = c.encode().unwrap();
    let back = CachedProfile::decode(&bytes).unwrap();
    assert_eq!(back, c);
    assert!(c.is_fresh(1_500, 600));
    assert!(!c.is_fresh(1_700, 600));
    assert!(
        !c.is_fresh(500, 600),
        "an entry from the future is not fresh"
    );
    assert_eq!(choose_source(Some(&c), 1_100, 600), ProfileSource::Cache);
    assert_eq!(choose_source(Some(&c), 9_000, 600), ProfileSource::Fetch);
    assert_eq!(choose_source(None, 0, 600), ProfileSource::Fetch);

    assert_eq!(
        CachedProfile::decode(b"{").unwrap_err().kind,
        ErrorKind::Integrity
    );
    let tampered = String::from_utf8(bytes.clone())
        .unwrap()
        .replace("1111111111111111111111111111111111111111", "xyz");
    assert_eq!(
        CachedProfile::decode(tampered.as_bytes()).unwrap_err().kind,
        ErrorKind::Integrity
    );
    let other_format =
        String::from_utf8(bytes)
            .unwrap()
            .replacen("\"format\": 1", "\"format\": 9", 1);
    assert_eq!(
        CachedProfile::decode(other_format.as_bytes())
            .unwrap_err()
            .kind,
        ErrorKind::Integrity
    );
}

#[test]
fn cache_keys_are_safe() {
    assert_eq!(
        profile_cache_key(Fabric, "1.21.11", "0.20.1").unwrap(),
        "loader/net.fabricmc.fabric-loader/1.21.11/0.20.1.json"
    );
    for (g, l) in [("..", "1"), ("1", "a/b"), ("", "1"), ("1", "a\\b")] {
        assert!(profile_cache_key(Fabric, g, l).is_err());
    }
    assert!(profile_cache_key(vl_core::component::ComponentKind::Game, "1", "1").is_err());
}

#[test]
fn offline_launch_uses_the_cache() {
    let cached = CachedProfile::new(fabric(), Some("\"e1\"".into()), 100);
    let down = || FetchOutcome::Failed(Error::new(ErrorKind::Network, "upstream unreachable"));

    // Upstream down: the stale cache entry is used as is.
    let r = resolve_profile(
        Fabric,
        "1.21.11",
        "0.20.1",
        Some(cached.clone()),
        down(),
        9_999,
    )
    .unwrap();
    assert_eq!(r, cached);
    // Nothing cached: the network error comes through unchanged.
    let e = resolve_profile(Fabric, "1.21.11", "0.20.1", None, down(), 9_999).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Network);

    // 304: the entry is revalidated (timestamp moves, ETag stays).
    let r = resolve_profile(
        Fabric,
        "1.21.11",
        "0.20.1",
        Some(cached.clone()),
        FetchOutcome::NotModified,
        500,
    )
    .unwrap();
    assert_eq!((r.fetched_unix, r.etag.as_deref()), (500, Some("\"e1\"")));
    assert!(
        resolve_profile(
            Fabric,
            "1.21.11",
            "0.20.1",
            None,
            FetchOutcome::NotModified,
            1
        )
        .is_err()
    );

    // A fresh document replaces the cache; a damaged one falls back to it, or fails clearly.
    let fresh = FetchOutcome::Fresh(loader_fixture("fabric_profile.json"), Some("\"e2\"".into()));
    let r = resolve_profile(
        Fabric,
        "1.21.11",
        "0.20.1",
        Some(cached.clone()),
        fresh,
        700,
    )
    .unwrap();
    assert_eq!((r.fetched_unix, r.etag.as_deref()), (700, Some("\"e2\"")));
    let bad = || FetchOutcome::Fresh(b"{\"id\":\"x\"}".to_vec(), None);
    let r = resolve_profile(
        Fabric,
        "1.21.11",
        "0.20.1",
        Some(cached.clone()),
        bad(),
        800,
    )
    .unwrap();
    assert_eq!(r, cached);
    let e = resolve_profile(Fabric, "1.21.11", "0.20.1", None, bad(), 800).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Integrity);
}

#[test]
fn per_game_lists_read_the_nested_loader_object() {
    let doc = br#"[
        {"loader": {"version": "0.19.0-beta.1", "stable": false}, "intermediary": {}},
        {"loader": {"version": "0.18.4", "stable": true, "build": 9}, "hashed": {}},
        {"loader": {"version": "0.18.3"}}
    ]"#;
    let list = LoaderVersions::parse_for_game(Fabric, "1.21.1", doc).unwrap();
    let versions: Vec<_> = list
        .versions
        .iter()
        .map(|v| (&*v.version, v.stable))
        .collect();
    assert_eq!(
        versions,
        [("0.19.0-beta.1", false), ("0.18.4", true), ("0.18.3", true)]
    );
    assert_eq!(list.recommended().unwrap().version, "0.18.4");

    assert!(
        LoaderVersions::parse_for_game(Fabric, "1.2.5", b"[]")
            .unwrap()
            .versions
            .is_empty()
    );
    for bad in [&b"{}"[..], b"[{\"version\": \"1\"}]", b"nope"] {
        let e = LoaderVersions::parse_for_game(Fabric, "1.21.1", bad).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Integrity);
    }
}
