//! `zaap.yml` rendering (Nunjucks template → YAML), reproducing Zaap's `configuration.compile`
//! (main.js L52445-52640): render, parse, refresh settings from the parsed `settings:` defs, repeat until
//! the rendered text is stable (max 5 iterations), then shallow-merge over the Zaap defaults.
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::settings::GameSettings;

/// Zaap default `executable`: run `zaap-start.bat` from the install dir.
pub const ZAAP_START: &str = "zaap-start";
const MAX_COMPILE_ITERATIONS: usize = 5;

/// One `settings:` entry of `zaap.yml`, exposed as-is to the UI.
/// `label`, `description` and option labels are either a string or a `{lang: text}` map.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettingDef {
    pub name: String,
    #[serde(default)]
    pub default: Value,
    #[serde(default)]
    pub label: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Value>,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub options: Vec<SettingOption>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettingOption {
    #[serde(default)]
    pub label: Value,
    #[serde(default)]
    pub value: Value,
}

/// `os` context object (Node `os.*` equivalents).
#[derive(Clone, Debug, Serialize)]
pub struct OsInfo {
    pub platform: String,
    pub arch: String,
    pub release: String,
    pub cores: usize,
    pub homedir: String,
    pub tmpdir: String,
    pub ram: u64,
}

impl OsInfo {
    pub fn current() -> Self {
        let platform = match std::env::consts::OS {
            "windows" => "windows",
            "macos" => "darwin",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            other => other,
        };
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        OsInfo {
            platform: platform.into(),
            arch: arch.into(),
            release: sysinfo::System::kernel_version().unwrap_or_default(),
            cores: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            homedir: dirs::home_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            tmpdir: std::env::temp_dir().display().to_string(),
            ram: sys.total_memory(),
        }
    }
}

/// Template context. `settings` holds the *stored* per-release values; defaults from the rendered
/// `settings:` defs are applied during compilation.
#[derive(Clone, Debug)]
pub struct RenderContext {
    pub os: OsInfo,
    /// Launcher settings (`zaap.*`), at least `LANGUAGE`.
    pub zaap: BTreeMap<String, Value>,
    /// `env.*`: the ZAAP_* vars of the instance being launched (launch-time compile only).
    pub env: Option<BTreeMap<String, String>>,
    pub settings: GameSettings,
}

impl RenderContext {
    pub fn new(language: &str, settings: GameSettings) -> Self {
        let mut zaap = BTreeMap::new();
        zaap.insert("LANGUAGE".to_string(), Value::String(language.to_string()));
        RenderContext {
            os: OsInfo::current(),
            zaap,
            env: None,
            settings,
        }
    }
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = Some(env);
        self
    }
}

/// Compiled game configuration.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ZaapConfig {
    /// `zaap-start` (default) or a path relative to the install dir.
    pub executable: String,
    pub arguments: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Deduplicated, `configuration` first.
    pub fragments: Vec<String>,
    pub settings: Vec<SettingDef>,
    /// Effective setting values (stored values validated against defs, defaults otherwise).
    pub settings_values: BTreeMap<String, Value>,
    /// `None` = unlimited (`maxInstances: true`).
    pub max_instances: Option<u32>,
    pub has_log_button: bool,
}

impl Default for ZaapConfig {
    fn default() -> Self {
        ZaapConfig {
            executable: ZAAP_START.into(),
            arguments: vec![],
            env: BTreeMap::new(),
            fragments: vec!["configuration".into(), "main".into()],
            settings: vec![],
            settings_values: BTreeMap::new(),
            max_instances: Some(1),
            has_log_button: true,
        }
    }
}

/// Render `<install_dir>/zaap.yml`. A missing file yields the Zaap defaults (like Zaap).
pub fn render_zaap_config(install_dir: &Path, ctx: &RenderContext) -> anyhow::Result<ZaapConfig> {
    let main_path = install_dir.join("zaap.yml");
    let source = match std::fs::read_to_string(&main_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ZaapConfig::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", main_path.display())),
    };
    let source = preprocess_template(&source);

    let mut env = minijinja::Environment::new();
    let root: PathBuf = install_dir.to_path_buf();
    env.set_loader(move |name| load_template(&root, name));

    let mut values: BTreeMap<String, Value> = ctx.settings.values.clone();
    let mut defs: Vec<SettingDef> = vec![];
    let mut previous: Option<String> = None;
    let mut parsed = Value::Null;
    for iteration in 0.. {
        if iteration >= MAX_COMPILE_ITERATIONS {
            bail!("zaap.yml: compile iteration count exceeded");
        }
        let context = serde_json::json!({
            "os": ctx.os,
            "zaap": ctx.zaap,
            "env": ctx.env,
            "game": { "location": install_dir.display().to_string(), "settings": values },
        });
        let tmpl = env
            .template_from_named_str("zaap.yml", &source)
            .context("zaap.yml: template syntax")?;
        let rendered = tmpl
            .render(minijinja::Value::from(minijinja::value::Serde(&context)))
            .map_err(|e| anyhow!("zaap.yml: render failed: {e:#}"))?;
        if previous.as_deref() == Some(rendered.as_str()) {
            break;
        }
        parsed = serde_yaml_ng::from_str::<Value>(&rendered)
            .context("zaap.yml: rendered output is not valid YAML")?;
        defs = parse_setting_defs(&parsed)?;
        values = ctx.settings.effective(&defs);
        previous = Some(rendered);
    }
    build_config(&parsed, defs, values)
}

fn load_template(root: &Path, name: &str) -> Result<Option<String>, minijinja::Error> {
    let rel = Path::new(name);
    // Only plain relative paths inside the install dir.
    if name.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Ok(None);
    }
    match std::fs::read_to_string(root.join(rel)) {
        Ok(s) => Ok(Some(preprocess_template(&s))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(minijinja::Error::new(
            minijinja::ErrorKind::InvalidOperation,
            "cannot read template",
        )
        .with_source(e)),
    }
}

/// Nunjucks accepts attribute names starting with a digit (`game.settings.4kReady`); Jinja does not.
/// Inside `{{ }}` / `{% %}` tags, rewrite `ident.4kReady` into `ident["4kReady"]`.
pub fn preprocess_template(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = find_tag_start(rest) {
        let (before, tag_and_after) = rest.split_at(start);
        out.push_str(before);
        let close = if tag_and_after.starts_with("{{") {
            "}}"
        } else {
            "%}"
        };
        let end = tag_and_after[2..]
            .find(close)
            .map(|i| i + 2 + close.len())
            .unwrap_or(tag_and_after.len());
        out.push_str(&rewrite_digit_attrs(&tag_and_after[..end]));
        rest = &tag_and_after[end..];
    }
    out.push_str(rest);
    out
}

fn find_tag_start(s: &str) -> Option<usize> {
    match (s.find("{{"), s.find("{%")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn rewrite_digit_attrs(tag: &str) -> String {
    let b = tag.as_bytes();
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = String::with_capacity(tag.len() + 8);
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            out.push(c as char);
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            quote = Some(c);
        }
        if c == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit() && i > 0 {
            // Preceding token must be an identifier (not a number like `1.5`), or `]` / `)`.
            let mut j = i;
            while j > 0 && is_ident(b[j - 1]) {
                j -= 1;
            }
            let prev_is_name = (j < i && !b[j].is_ascii_digit()) || matches!(b[i - 1], b']' | b')');
            if prev_is_name {
                let mut k = i + 1;
                while k < b.len() && is_ident(b[k]) {
                    k += 1;
                }
                if b[i + 1..k].iter().any(|c| !c.is_ascii_digit()) {
                    out.push_str("[\"");
                    out.push_str(&tag[i + 1..k]);
                    out.push_str("\"]");
                    i = k;
                    continue;
                }
            }
        }
        // Push the full UTF-8 char.
        let ch = tag[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn parse_setting_defs(parsed: &Value) -> anyhow::Result<Vec<SettingDef>> {
    match parsed.get("settings") {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value(v.clone()).context("zaap.yml: invalid settings entry"))
            .collect(),
        Some(_) => bail!("zaap.yml: `settings` must be a list"),
    }
}

fn scalar_to_string(v: &Value, what: &str) -> anyhow::Result<String> {
    Ok(match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => bail!("zaap.yml: {what} must be a scalar"),
    })
}

fn build_config(
    parsed: &Value,
    settings: Vec<SettingDef>,
    settings_values: BTreeMap<String, Value>,
) -> anyhow::Result<ZaapConfig> {
    let mut cfg = ZaapConfig {
        settings,
        settings_values,
        ..ZaapConfig::default()
    };
    let obj = match parsed {
        Value::Object(o) => o,
        Value::Null => return Ok(cfg),
        _ => bail!("zaap.yml: top level must be a mapping"),
    };
    if let Some(v) = obj.get("executable").filter(|v| !v.is_null()) {
        cfg.executable = scalar_to_string(v, "executable")?;
    }
    if let Some(v) = obj.get("arguments").filter(|v| !v.is_null()) {
        let arr = v
            .as_array()
            .ok_or_else(|| anyhow!("zaap.yml: `arguments` must be a list"))?;
        cfg.arguments = arr
            .iter()
            .map(|a| scalar_to_string(a, "argument"))
            .collect::<anyhow::Result<_>>()?;
    }
    if let Some(v) = obj.get("env").filter(|v| !v.is_null()) {
        let map = v
            .as_object()
            .ok_or_else(|| anyhow!("zaap.yml: `env` must be a mapping"))?;
        cfg.env = map
            .iter()
            .map(|(k, v)| Ok((k.clone(), scalar_to_string(v, "env value")?)))
            .collect::<anyhow::Result<_>>()?;
    }
    let mut fragments: Vec<String> = match obj.get("fragments").filter(|v| !v.is_null()) {
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|a| scalar_to_string(a, "fragment"))
            .collect::<anyhow::Result<_>>()?,
        Some(_) => bail!("zaap.yml: `fragments` must be a list"),
        None => vec!["main".into()],
    };
    fragments.retain(|f| !f.is_empty() && f != "configuration");
    fragments.insert(0, "configuration".into());
    let mut seen = std::collections::HashSet::new();
    fragments.retain(|f| seen.insert(f.clone()));
    cfg.fragments = fragments;
    cfg.max_instances = match obj.get("maxInstances") {
        None | Some(Value::Null) => Some(1),
        Some(Value::Bool(true)) => None,
        Some(Value::Bool(false)) => Some(1),
        Some(Value::Number(n)) => Some(
            n.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(1)
                .max(1),
        ),
        Some(_) => bail!("zaap.yml: invalid `maxInstances`"),
    };
    if let Some(Value::Bool(b)) = obj.get("hasLogButton") {
        cfg.has_log_button = *b;
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_rewrites_digit_attributes_only_in_tags() {
        let src = "a.4kReady {% if game.settings.4kReady %}x{{ 1.5 }}{{ a['b'].2x }}{{ \"s.4k\" }}{% endif %}";
        assert_eq!(
            preprocess_template(src),
            "a.4kReady {% if game.settings[\"4kReady\"] %}x{{ 1.5 }}{{ a['b'][\"2x\"] }}{{ \"s.4k\" }}{% endif %}"
        );
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = render_zaap_config(
            dir.path(),
            &RenderContext::new("fr", GameSettings::default()),
        )
        .unwrap();
        assert_eq!(cfg, ZaapConfig::default());
    }

    #[test]
    fn loader_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_template(dir.path(), "../x.yml").unwrap().is_none());
        assert!(load_template(dir.path(), "C:/x.yml").unwrap().is_none());
    }
}
