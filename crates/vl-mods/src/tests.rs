use super::*;

const FABRIC: &str = r#"{
  "schemaVersion": 1, "id": "examplemod", "version": "1.2.3", "name": "Example Mod",
  "description": "Does things.",
  "authors": ["Alice", {"name": "Bob", "contact": {}}],
  "icon": {"16": "assets/a16.png", "128": "assets/a128.png"},
  "depends": {"minecraft": ">=1.20 <1.21", "fabricloader": "*", "other": ["1.0", "2.0"]},
  "recommends": {"nice": "*"},
  "breaks": {"bad": "<1"},
  "jars": [{"file": "META-INF/jars/lib.jar"}]
}"#;

#[test]
fn parses_fabric() {
    let m = parse_fabric(FABRIC.as_bytes()).unwrap();
    assert_eq!(m.id, "examplemod");
    assert_eq!(m.name, "Example Mod");
    assert_eq!(m.version.as_deref(), Some("1.2.3"));
    assert_eq!(m.authors, ["Alice", "Bob"]);
    assert_eq!(m.icon.as_deref(), Some("assets/a128.png"));
    assert_eq!(m.game_versions.as_deref(), Some(">=1.20 <1.21"));
    let other = m.dependencies.iter().find(|d| d.id == "other").unwrap();
    assert_eq!(other.versions.as_deref(), Some("1.0 || 2.0"));
    let kind = |id: &str| m.dependencies.iter().find(|d| d.id == id).map(|d| d.kind);
    assert_eq!(kind("nice"), Some(DependencyKind::Optional));
    assert_eq!(kind("bad"), Some(DependencyKind::Conflicts));
}

#[test]
fn fabric_without_id_or_with_garbage_is_an_error() {
    assert!(parse_fabric(br#"{"name": "x"}"#).is_err());
    assert!(parse_fabric(b"not json").is_err());
    assert!(parse_fabric(b"").is_err());
    assert!(parse_fabric(&[0xff, 0xfe, 0x00]).is_err());
    // Wrong field types are ignored, not fatal.
    let m = parse_fabric(br#"{"id":"a","authors":5,"depends":[],"icon":3,"name":7}"#).unwrap();
    assert_eq!(m.name, "a");
    assert!(m.authors.is_empty() && m.dependencies.is_empty() && m.icon.is_none());
}

#[test]
fn fabric_survives_a_byte_order_mark() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(FABRIC.as_bytes());
    assert!(parse_fabric(&bytes).is_ok());
}

#[test]
fn parses_quilt() {
    let json = r#"{"schema_version":1,"quilt_loader":{
      "id":"qmod","version":"0.5","group":"g",
      "metadata":{"name":"Q Mod","description":"d","contributors":{"Zed":"Developer","Amy":"Owner"},"icon":"q.png"},
      "depends":["plain",{"id":"minecraft","versions":">=1.20"},{"id":"opt","optional":true}],
      "breaks":[{"id":"clash","versions":"<2"}]}}"#;
    let m = parse_quilt(json.as_bytes()).unwrap();
    assert_eq!((m.id.as_str(), m.name.as_str()), ("qmod", "Q Mod"));
    assert_eq!(m.authors, ["Amy", "Zed"]);
    assert_eq!(m.icon.as_deref(), Some("q.png"));
    assert_eq!(m.game_versions.as_deref(), Some(">=1.20"));
    assert_eq!(m.dependencies.len(), 4);
    assert!(parse_quilt(br#"{"id":"x"}"#).is_err());
}

#[test]
fn parses_forge_and_neoforge() {
    let toml = r#"
modLoader = "javafml"
loaderVersion = "[47,)"
license = "MIT"
authors = "Carol, Dan"
logoFile = "logo.png"
[[mods]]
modId = "forgemod"
version = "${file.jarVersion}"
displayName = "Forge Mod"
description = "Multi-line."
[[mods]]
modId = "second"
[[dependencies.forgemod]]
modId = "minecraft"
mandatory = true
versionRange = "[1.20,1.21)"
[[dependencies.forgemod]]
modId = "extra"
mandatory = false
[[dependencies.forgemod]]
modId = "neo"
type = "incompatible"
"#;
    let mut mods = parse_mods_toml(toml.as_bytes(), ModLoader::Forge).unwrap();
    assert_eq!(mods.len(), 2);
    let m = &mods[0];
    assert_eq!(m.authors, ["Carol", "Dan"]);
    assert_eq!(m.icon.as_deref(), Some("logo.png"));
    assert_eq!(m.game_versions.as_deref(), Some("[1.20,1.21)"));
    assert_eq!(m.dependencies[1].kind, DependencyKind::Optional);
    assert_eq!(m.dependencies[2].kind, DependencyKind::Conflicts);
    assert_eq!(mods[1].name, "second");
    resolve_jar_version(&mut mods, Some("4.5"));
    assert_eq!(mods[0].version.as_deref(), Some("4.5"));
    let neo = parse_mods_toml(toml.as_bytes(), ModLoader::NeoForge).unwrap();
    assert_eq!(neo[0].loader, ModLoader::NeoForge);
}

#[test]
fn bad_toml_is_an_error() {
    assert!(parse_mods_toml(b"[[mods", ModLoader::Forge).is_err());
    assert!(parse_mods_toml(b"x = 1", ModLoader::Forge).is_err());
    assert!(parse_mods_toml(b"[[mods]]\nversion = \"1\"", ModLoader::Forge).is_err());
    assert!(parse_mods_toml(&[0xff, 0xff], ModLoader::NeoForge).is_err());
}

#[test]
fn parses_legacy_mcmod_info() {
    let list = r#"[{"modid":"old","name":"Old Mod","version":"1.0","mcversion":"1.7.10",
      "description":"d","authorList":["Eve"],"logoFile":"l.png","requiredMods":["Forge@10","base"]}]"#;
    let mods = parse_mcmod_info(list.as_bytes()).unwrap();
    assert_eq!(mods[0].game_versions.as_deref(), Some("1.7.10"));
    assert_eq!(mods[0].dependencies[0].id, "Forge");
    assert_eq!(mods[0].authors, ["Eve"]);
    let wrapped = r#"{"modListVersion":2,"modList":[{"modid":"w","authors":["X"]}]}"#;
    assert_eq!(
        parse_mcmod_info(wrapped.as_bytes()).unwrap()[0].authors,
        ["X"]
    );
    assert!(parse_mcmod_info(b"[]").is_err());
    assert!(parse_mcmod_info(b"{").is_err());
}

#[test]
fn dispatches_by_path_and_reads_the_manifest() {
    assert!(
        parse_file("fabric.mod.json", FABRIC.as_bytes())
            .unwrap()
            .is_ok()
    );
    assert!(parse_file("other.txt", b"").is_none());
    let manifest =
        b"Manifest-Version: 1.0\r\nImplementation-Ti\r\n tle: x\r\nImplementation-Version: 9.8\r\n";
    assert_eq!(manifest_version(manifest).as_deref(), Some("9.8"));
    assert_eq!(manifest_version(b"x: y"), None);
}
