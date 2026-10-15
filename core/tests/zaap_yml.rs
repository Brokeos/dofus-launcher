//! Renders the real Dofus 3 `zaap.yml` + `zaap-windows.yml` (cytrus configuration fragment).
use std::collections::BTreeMap;
use std::path::PathBuf;

use dofus_core::game::{
    GameSessions, GameSettings, RenderContext, ZaapConfig, render_zaap_config, zaap_env,
};
use serde_json::json;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dofus3")
}

fn ctx(settings: GameSettings, env: Option<BTreeMap<String, String>>) -> RenderContext {
    let mut c = RenderContext::new("fr", settings);
    c.os.platform = "windows".into();
    c.env = env;
    c
}

fn render(
    values: &[(&str, serde_json::Value)],
    env: Option<BTreeMap<String, String>>,
) -> ZaapConfig {
    let settings = GameSettings {
        values: values
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    };
    render_zaap_config(&fixtures(), &ctx(settings, env)).unwrap()
}

fn test_env() -> (BTreeMap<String, String>, String, u32) {
    let s = GameSessions::new().create("dofus3", Some(123));
    (
        zaap_env(&s, Some(std::path::Path::new(r"C:\logs\dofus-dofus3"))),
        s.hash,
        s.instance_id,
    )
}

fn expected_args(hash: &str, id: u32, extra: &[&str], dx: &str) -> Vec<String> {
    let id = id.to_string();
    let mut v: Vec<&str> = vec![
        "--port",
        "26116",
        "--gameName",
        "dofus",
        "--gameRelease",
        "dofus3",
        "--instanceId",
        &id,
        "--hash",
        hash,
        "--canLogin",
        "true",
        "--langCode",
        "fr",
        "--autoConnectType",
        "1",
        "--connectionPort",
        "5555",
    ];
    v.extend_from_slice(extra);
    v.push(dx);
    v.into_iter().map(String::from).collect()
}

const BASE_FRAGMENTS: &[&str] = &[
    "configuration",
    "win32_x64",
    "start-script",
    "bones",
    "skins",
    "audio",
    "data",
    "i18n",
    "picto",
    "map_common",
];

#[test]
fn dofus3_defaults() {
    let (env, hash, id) = test_env();
    let cfg = render(&[], Some(env));
    assert_eq!(cfg.executable, "zaap-start");
    assert_eq!(cfg.max_instances, None);
    assert!(cfg.has_log_button);
    assert!(cfg.env.is_empty());
    assert_eq!(cfg.arguments, expected_args(&hash, id, &[], ""));
    assert_eq!(cfg.fragments, BASE_FRAGMENTS);
    let names: Vec<_> = cfg.settings.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "language",
            "autoConnectType",
            "connectionPort",
            "directxVersion",
            "hdReady",
            "4kReady"
        ]
    );
    // Rendered defaults.
    assert_eq!(cfg.settings[0].default, json!("fr"));
    assert_eq!(cfg.settings[0].label["fr"], json!("Langue"));
    assert_eq!(cfg.settings[1].options[2].value, json!(2));
    assert_eq!(cfg.settings[4].kind, "checkbox");
    assert_eq!(
        cfg.settings_values,
        BTreeMap::from([
            ("language".into(), json!("fr")),
            ("autoConnectType".into(), json!(1)),
            ("connectionPort".into(), json!("5555")),
            ("directxVersion".into(), json!("")),
            ("hdReady".into(), json!(false)),
            ("4kReady".into(), json!(false)),
        ])
    );
}

#[test]
fn dofus3_hd_and_4k_and_dx11() {
    let (env, hash, id) = test_env();
    let cfg = render(
        &[
            ("hdReady", json!(true)),
            ("directxVersion", json!("-force-d3d11")),
        ],
        Some(env.clone()),
    );
    assert_eq!(
        cfg.arguments,
        expected_args(&hash, id, &["--hdReady"], "-force-d3d11")
    );
    let mut frags: Vec<&str> = BASE_FRAGMENTS.to_vec();
    frags.push("map_textures_2x");
    assert_eq!(cfg.fragments, frags);

    let cfg = render(
        &[("hdReady", json!(true)), ("4kReady", json!(true))],
        Some(env),
    );
    assert_eq!(
        cfg.arguments,
        expected_args(&hash, id, &["--hdReady", "--4kReady"], "")
    );
    frags.push("map_textures_4x");
    assert_eq!(cfg.fragments, frags);
}

#[test]
fn dofus3_without_env_and_invalid_stored_values() {
    // Settings-UI compile (no env): ZAAP_* render empty. Invalid option values fall back to defaults.
    let cfg = render(
        &[
            ("connectionPort", json!("1")),
            ("language", json!("en")),
            ("autoConnectType", json!(2)),
        ],
        None,
    );
    assert_eq!(cfg.arguments[1], "");
    assert_eq!(cfg.settings_values["connectionPort"], json!("5555"));
    assert_eq!(
        &cfg.arguments[13..18],
        ["en", "--autoConnectType", "2", "--connectionPort", "5555"]
    );
}
