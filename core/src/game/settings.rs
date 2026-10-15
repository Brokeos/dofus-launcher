//! Per-release game settings (Zaap `gameSettings`, main.js L35540-35690), persisted atomically in
//! `data_dir()/game-settings-<release>.json` as a flat `{name: value}` JSON object.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::SettingDef;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettingError {
    #[error("unknown game setting `{0}`")]
    UnknownKey(String),
    #[error("invalid value for game setting `{0}`")]
    InvalidValue(String),
}

/// Stored (user-chosen) values only; defaults come from the `zaap.yml` setting defs.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct GameSettings {
    pub values: BTreeMap<String, Value>,
}

impl GameSettings {
    pub fn path(release: &str) -> PathBuf {
        crate::paths::data_dir().join(format!("game-settings-{release}.json"))
    }
    pub fn load(release: &str) -> anyhow::Result<Self> {
        Self::load_from(&Self::path(release))
    }
    pub fn save(&self, release: &str) -> anyhow::Result<()> {
        self.save_to(&Self::path(release))
    }
    /// Missing file → empty settings. A corrupt file is an error (never silently reset).
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        crate::paths::write_atomic(path, &bytes)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// Effective values for `defs`: stored value when valid, else the def default. Like Zaap's
    /// `checkOptions`, a truthy stored value not among the options is reset to the default.
    pub fn effective(&self, defs: &[SettingDef]) -> BTreeMap<String, Value> {
        defs.iter()
            .map(|d| {
                let v = match self.values.get(&d.name) {
                    Some(v) if is_valid(d, v) => v.clone(),
                    _ => d.default.clone(),
                };
                (d.name.clone(), v)
            })
            .collect()
    }

    /// Set one value after validating it against `defs` (key must exist; select values must be options).
    pub fn set_validated(
        &mut self,
        defs: &[SettingDef],
        key: &str,
        value: Value,
    ) -> Result<(), SettingError> {
        let def = defs
            .iter()
            .find(|d| d.name == key)
            .ok_or_else(|| SettingError::UnknownKey(key.into()))?;
        if !is_valid(def, &value) {
            return Err(SettingError::InvalidValue(key.into()));
        }
        self.values.insert(key.into(), value);
        Ok(())
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn is_valid(def: &SettingDef, v: &Value) -> bool {
    if def.kind == "checkbox" && !v.is_boolean() {
        return false;
    }
    !truthy(v) || def.options.is_empty() || def.options.iter().any(|o| &o.value == v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn defs() -> Vec<SettingDef> {
        serde_json::from_value(json!([
            {"name": "connectionPort", "default": "5555", "type": "select",
             "options": [{"label": "5555", "value": "5555"}, {"label": "443", "value": "443"}]},
            {"name": "hdReady", "default": false, "type": "checkbox"}
        ]))
        .unwrap()
    }

    #[test]
    fn effective_applies_defaults_and_resets_invalid() {
        let mut s = GameSettings::default();
        assert_eq!(
            s.effective(&defs()),
            BTreeMap::from([
                ("connectionPort".into(), json!("5555")),
                ("hdReady".into(), json!(false))
            ])
        );
        s.values.insert("connectionPort".into(), json!("1234"));
        s.values.insert("hdReady".into(), json!(true));
        s.values.insert("stale".into(), json!(1));
        let eff = s.effective(&defs());
        assert_eq!(eff["connectionPort"], json!("5555"));
        assert_eq!(eff["hdReady"], json!(true));
        assert!(!eff.contains_key("stale"));
    }

    #[test]
    fn set_validated_and_atomic_roundtrip() {
        let mut s = GameSettings::default();
        assert_eq!(
            s.set_validated(&defs(), "nope", json!(1)),
            Err(SettingError::UnknownKey("nope".into()))
        );
        assert_eq!(
            s.set_validated(&defs(), "connectionPort", json!(443)),
            Err(SettingError::InvalidValue("connectionPort".into()))
        );
        s.set_validated(&defs(), "connectionPort", json!("443"))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("game-settings-dofus3.json");
        assert_eq!(
            GameSettings::load_from(&p).unwrap(),
            GameSettings::default()
        );
        s.save_to(&p).unwrap();
        assert_eq!(GameSettings::load_from(&p).unwrap(), s);
    }
}
