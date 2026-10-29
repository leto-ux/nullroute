use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Standard PipeWire channel maps (`audio.position`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelMap {
    Mono,
    #[default]
    Stereo,
    Surround21,
    Quad,
    Surround51,
    Surround71,
}

impl ChannelMap {
    /// Returns the PipeWire `audio.position` string representation.
    pub fn as_pw_str(&self) -> &'static str {
        match self {
            ChannelMap::Mono => "MONO",
            ChannelMap::Stereo => "FL,FR",
            ChannelMap::Surround21 => "FL,FR,LFE",
            ChannelMap::Quad => "FL,FR,RL,RR",
            ChannelMap::Surround51 => "FL,FR,FC,LFE,RL,RR",
            ChannelMap::Surround71 => "FL,FR,FC,LFE,RL,RR,SL,SR",
        }
    }

    /// Returns the individual channel suffixes used on PipeWire ports (e.g. `playback_FL`, `capture_FR`).
    pub fn channels(&self) -> &'static [&'static str] {
        match self {
            ChannelMap::Mono => &["MONO"],
            ChannelMap::Stereo => &["FL", "FR"],
            ChannelMap::Surround21 => &["FL", "FR", "LFE"],
            ChannelMap::Quad => &["FL", "FR", "RL", "RR"],
            ChannelMap::Surround51 => &["FL", "FR", "FC", "LFE", "RL", "RR"],
            ChannelMap::Surround71 => &["FL", "FR", "FC", "LFE", "RL", "RR", "SL", "SR"],
        }
    }

    /// Parses a PipeWire `audio.position` value (e.g. `"FL,FR"` or `"[ FL FR ]"`).
    pub fn from_pw_str(raw: &str) -> Self {
        let cleaned: Vec<String> = raw
            .trim_matches(|c| c == '[' || c == ']' || c == '"' || c == '\'')
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_uppercase())
            .collect();

        let refs: Vec<&str> = cleaned.iter().map(|s| s.as_str()).collect();
        match refs.as_slice() {
            ["MONO"] => ChannelMap::Mono,
            ["FL", "FR"] => ChannelMap::Stereo,
            ["FL", "FR", "LFE"] => ChannelMap::Surround21,
            ["FL", "FR", "RL", "RR"] => ChannelMap::Quad,
            ["FL", "FR", "FC", "LFE", "RL", "RR"] => ChannelMap::Surround51,
            ["FL", "FR", "FC", "LFE", "RL", "RR", "SL", "SR"] => ChannelMap::Surround71,
            _ => ChannelMap::Stereo,
        }
    }
}

impl std::fmt::Display for ChannelMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_pw_str())
    }
}

impl std::str::FromStr for ChannelMap {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mono" | "1.0" => Ok(ChannelMap::Mono),
            "stereo" | "2.0" | "fl,fr" => Ok(ChannelMap::Stereo),
            "surround21" | "2.1" | "fl,fr,lfe" => Ok(ChannelMap::Surround21),
            "quad" | "4.0" | "fl,fr,rl,rr" => Ok(ChannelMap::Quad),
            "surround51" | "5.1" | "fl,fr,fc,lfe,rl,rr" => Ok(ChannelMap::Surround51),
            "surround71" | "7.1" | "fl,fr,fc,lfe,rl,rr,sl,sr" => Ok(ChannelMap::Surround71),
            other => bail!(
                "Unknown channel map '{}'. Expected one of: mono, stereo, 2.1, quad, 5.1, 7.1",
                other
            ),
        }
    }
}

/// Canonical topology configuration persisted in `~/.config/nullroute/topology.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyConfig {
    #[serde(default)]
    pub masters: Vec<MasterSink>,
    #[serde(default)]
    pub slaves: Vec<SlaveSink>,
    #[serde(default)]
    pub rules: Vec<RoutingRule>,
}

/// A Master virtual sink (`Audio/Duplex` null sink) whose monitor/capture ports feed hardware sinks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MasterSink {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<u32>,
    #[serde(default)]
    pub channel_map: ChannelMap,
    #[serde(default)]
    pub hardware_targets: Vec<String>,
}

/// A Slave virtual sink (`Audio/Duplex` null sink) representing an app category/channel (e.g., Browser, Games, Music, Comms, Others).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlaveSink {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub channel_map: ChannelMap,
    #[serde(default)]
    pub masters: Vec<String>,
    #[serde(default)]
    pub is_default_sink: bool,
    #[serde(default)]
    pub is_catch_all: bool,
}

/// A routing rule that directs matching application streams (`target.object`) to a Slave sink.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingRule {
    pub id: String,
    pub target_slave: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_description: Option<String>,
    #[serde(default)]
    pub matches: Vec<MatchClause>,
}

/// A single match block within `stream.rules` (`matches = [ { ... } ]`).
/// All entries inside `conditions` are AND-ed together. Multiple values for the same key
/// are supported (e.g. `application.process.binary = "electron"` and `application.process.binary = "!dotcefnative"`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchClause {
    #[serde(flatten)]
    pub conditions: BTreeMap<String, Vec<String>>,
}

impl MatchClause {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_condition(&mut self, key: impl Into<String>, val: impl Into<String>) {
        let k = key.into();
        let v = val.into();
        let list = self.conditions.entry(k).or_default();
        if !list.contains(&v) {
            list.push(v);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }

    /// Returns true if this clause consists primarily of negated (`!`) conditions (typical of catch-all rules).
    pub fn is_predominantly_negated(&self) -> bool {
        let total: usize = self.conditions.values().map(|v| v.len()).sum();
        let negated: usize = self
            .conditions
            .values()
            .flat_map(|v| v.iter())
            .filter(|val| val.starts_with('!'))
            .count();
        total >= 4 && negated * 2 >= total
    }

    /// Evaluates whether a set of stream properties satisfies all conditions in this clause.
    pub fn matches_props(&self, props: &BTreeMap<String, String>) -> bool {
        if self.conditions.is_empty() {
            return false;
        }
        for (key, patterns) in &self.conditions {
            let actual = props.get(key).map(|s| s.as_str());
            for pat in patterns {
                if let Some(negated_pat) = pat.strip_prefix('!') {
                    if let Some(act) = actual {
                        if pattern_matches(negated_pat, act) {
                            return false;
                        }
                    }
                } else {
                    match actual {
                        Some(act) if pattern_matches(pat, act) => {}
                        _ => return false,
                    }
                }
            }
        }
        true
    }

    /// Formats this clause concisely for CLI / tree display.
    pub fn display_compact(&self) -> String {
        let mut parts = Vec::new();
        for (k, vals) in &self.conditions {
            let short_k = match k.as_str() {
                "application.process.binary" => "bin",
                "application.name" => "app",
                "node.name" => "node",
                "node.description" => "desc",
                "media.role" => "role",
                other => other,
            };
            for v in vals {
                parts.push(format!("{}=\"{}\"", short_k, v));
            }
        }
        parts.join(" && ")
    }
}

fn pattern_matches(pattern: &str, actual: &str) -> bool {
    if let Some(regex_like) = pattern.strip_prefix('~') {
        // Basic glob/prefix support for '~alsa_output.pci.*'
        let prefix = regex_like.trim_end_matches(".*").trim_start_matches('^');
        actual.contains(prefix)
    } else {
        pattern == actual
    }
}

/// Strips optional leading index prefixes like `"1. Master"` -> `"Master"`, `"2. Browser"` -> `"Browser"`.
pub fn normalize_sink_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some((num_part, rest)) = trimmed.split_once(". ") {
        if !num_part.is_empty() && num_part.chars().all(|c| c.is_ascii_digit()) {
            return rest.trim().to_string();
        }
    }
    trimmed.to_string()
}

impl TopologyConfig {
    /// Returns the default path to `~/.config/nullroute/topology.toml`.
    pub fn default_path() -> Result<PathBuf> {
        let base = dirs::config_dir().context("Could not determine user ~/.config directory")?;
        Ok(base.join("nullroute").join("topology.toml"))
    }

    /// Loads `TopologyConfig` from the given path, or returns an empty default if it does not exist.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::load(path)
    }

    /// Loads `TopologyConfig` from a TOML file.
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read topology config from {}", path.display()))?;
        let cfg: Self = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML in {}", path.display()))?;
        Ok(cfg)
    }

    /// Saves `TopologyConfig` to a TOML file, creating parent directories if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }
        let serialized =
            toml::to_string_pretty(self).context("Failed to serialize TopologyConfig to TOML")?;
        fs::write(path, serialized)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        Ok(())
    }

    pub fn find_master(&self, name: &str) -> Option<&MasterSink> {
        let norm = normalize_sink_name(name);
        self.masters
            .iter()
            .find(|m| normalize_sink_name(&m.name).eq_ignore_ascii_case(&norm))
    }

    pub fn find_master_mut(&mut self, name: &str) -> Option<&mut MasterSink> {
        let norm = normalize_sink_name(name);
        self.masters
            .iter_mut()
            .find(|m| normalize_sink_name(&m.name).eq_ignore_ascii_case(&norm))
    }

    pub fn find_slave(&self, name: &str) -> Option<&SlaveSink> {
        let norm = normalize_sink_name(name);
        self.slaves
            .iter()
            .find(|s| normalize_sink_name(&s.name).eq_ignore_ascii_case(&norm))
    }

    pub fn find_slave_mut(&mut self, name: &str) -> Option<&mut SlaveSink> {
        let norm = normalize_sink_name(name);
        self.slaves
            .iter_mut()
            .find(|s| normalize_sink_name(&s.name).eq_ignore_ascii_case(&norm))
    }

    pub fn rules_for_slave(&self, slave_name: &str) -> Vec<&RoutingRule> {
        let norm = normalize_sink_name(slave_name);
        self.rules
            .iter()
            .filter(|r| normalize_sink_name(&r.target_slave).eq_ignore_ascii_case(&norm))
            .collect()
    }

    /// Computes the automatic inverse match clause (`!val`) for a catch-all slave (like `Others`)
    /// by negating all positive single-condition and primary identifiers from non-catch-all slave rules,
    /// plus excluding internal loopback/bridge streams.
    pub fn compute_catch_all_clause(&self, catch_all_slave: &str) -> MatchClause {
        let norm_catch_all = normalize_sink_name(catch_all_slave);
        let mut clause = MatchClause::new();

        for rule in &self.rules {
            if normalize_sink_name(&rule.target_slave).eq_ignore_ascii_case(&norm_catch_all) {
                continue;
            }
            if let Some(desc) = &rule.set_description {
                clause.add_condition("node.description", format!("!{}", desc));
            }
            for m in &rule.matches {
                for (key, vals) in &m.conditions {
                    for v in vals {
                        if !v.starts_with('!') {
                            clause.add_condition(key.clone(), format!("!{}", v));
                        }
                    }
                }
            }
        }

        // Also exclude any explicit positive app matches defined on the catch-all slave itself
        // (e.g., if `mpv` is explicitly matched on `Others`, and `!mpv` was on `Music`, don't negate `mpv` in the catch-all clause).
        let mut explicit_catch_all_positives: BTreeSet<(String, String)> = BTreeSet::new();
        for rule in &self.rules {
            if normalize_sink_name(&rule.target_slave).eq_ignore_ascii_case(&norm_catch_all) {
                for m in &rule.matches {
                    for (k, vals) in &m.conditions {
                        for v in vals {
                            if !v.starts_with('!') {
                                explicit_catch_all_positives.insert((k.clone(), v.clone()));
                            }
                        }
                    }
                }
            }
        }

        for (k, v) in explicit_catch_all_positives {
            if let Some(list) = clause.conditions.get_mut(&k) {
                let neg = format!("!{}", v);
                list.retain(|item| item != &neg);
            }
        }

        // Exclude internal bridge/loopback nodes so nullroute's own links are never hijacked
        clause.add_condition("node.name", "!input.Master_Output_Bridge");
        clause.add_condition("node.name", "!output.Master_Output_Bridge");
        for master in &self.masters {
            clause.add_condition("node.name", format!("!{}", master.name));
        }
        for slave in &self.slaves {
            clause.add_condition("node.name", format!("!{}", slave.name));
        }

        clause
    }

    /// Evaluates active stream properties against all configured rules and returns the target Slave name.
    pub fn match_stream_target(&self, props: &BTreeMap<String, String>) -> Option<String> {
        // 1. Evaluate non-catch-all slave rules first
        for rule in &self.rules {
            let is_catch_all_slave = self
                .find_slave(&rule.target_slave)
                .map(|s| s.is_catch_all)
                .unwrap_or(false);
            if is_catch_all_slave {
                continue;
            }
            for clause in &rule.matches {
                if clause.matches_props(props) {
                    return Some(rule.target_slave.clone());
                }
            }
        }

        // 2. Evaluate explicit positive rules on catch-all slaves
        for rule in &self.rules {
            let is_catch_all_slave = self
                .find_slave(&rule.target_slave)
                .map(|s| s.is_catch_all)
                .unwrap_or(false);
            if !is_catch_all_slave {
                continue;
            }
            for clause in &rule.matches {
                if clause.matches_props(props) {
                    return Some(rule.target_slave.clone());
                }
            }
        }

        // 3. Fall back to catch-all slave if one is configured
        if let Some(catch_all) = self.slaves.iter().find(|s| s.is_catch_all) {
            return Some(catch_all.name.clone());
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_map_parsing_and_formatting() {
        assert_eq!(ChannelMap::default(), ChannelMap::Stereo);
        assert_eq!(ChannelMap::Stereo.as_pw_str(), "FL,FR");
        assert_eq!(ChannelMap::Stereo.channels(), &["FL", "FR"]);
        assert_eq!(ChannelMap::from_pw_str("[ FL FR ]"), ChannelMap::Stereo);
        assert_eq!(
            "5.1".parse::<ChannelMap>().unwrap(),
            ChannelMap::Surround51
        );
    }

    #[test]
    fn test_normalize_sink_name() {
        assert_eq!(normalize_sink_name("1. Master"), "Master");
        assert_eq!(normalize_sink_name("2. Browser"), "Browser");
        assert_eq!(normalize_sink_name("Games"), "Games");
    }

    #[test]
    fn test_stream_matching_and_catch_all() {
        let mut topo = TopologyConfig::default();
        topo.slaves.push(SlaveSink {
            name: "Browser".into(),
            description: None,
            channel_map: ChannelMap::Stereo,
            masters: vec!["Master".into()],
            is_default_sink: false,
            is_catch_all: false,
        });
        topo.slaves.push(SlaveSink {
            name: "Comms".into(),
            description: None,
            channel_map: ChannelMap::Stereo,
            masters: vec!["Master".into()],
            is_default_sink: false,
            is_catch_all: false,
        });
        topo.slaves.push(SlaveSink {
            name: "Others".into(),
            description: None,
            channel_map: ChannelMap::Stereo,
            masters: vec!["Master".into()],
            is_default_sink: true,
            is_catch_all: true,
        });

        let mut browser_clause = MatchClause::new();
        browser_clause.add_condition("node.name", "Chromium");
        browser_clause.add_condition("application.process.binary", "!vesktop");
        topo.rules.push(RoutingRule {
            id: "browser".into(),
            target_slave: "Browser".into(),
            set_description: None,
            matches: vec![browser_clause],
        });

        let mut comms_clause = MatchClause::new();
        comms_clause.add_condition("application.process.binary", "vesktop");
        topo.rules.push(RoutingRule {
            id: "comms".into(),
            target_slave: "Comms".into(),
            set_description: None,
            matches: vec![comms_clause],
        });

        // Chromium (not vesktop) -> Browser
        let mut chromium_props = BTreeMap::new();
        chromium_props.insert("node.name".into(), "Chromium".into());
        chromium_props.insert("application.process.binary".into(), "chromium".into());
        assert_eq!(
            topo.match_stream_target(&chromium_props).as_deref(),
            Some("Browser")
        );

        // Chromium inside vesktop -> Comms (excluded from Browser by !vesktop)
        let mut vesktop_props = BTreeMap::new();
        vesktop_props.insert("node.name".into(), "Chromium".into());
        vesktop_props.insert("application.process.binary".into(), "vesktop".into());
        assert_eq!(
            topo.match_stream_target(&vesktop_props).as_deref(),
            Some("Comms")
        );

        // Unknown app -> Others (catch-all)
        let mut unknown_props = BTreeMap::new();
        unknown_props.insert("application.name".into(), "vlc".into());
        assert_eq!(
            topo.match_stream_target(&unknown_props).as_deref(),
            Some("Others")
        );
    }
}

