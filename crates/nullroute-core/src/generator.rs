use crate::model::{MatchClause, SlaveSink, TopologyConfig};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Generates the `10-sinks.conf` file defining all Master and Slave virtual sinks in `context.objects`.
pub fn generate_sinks_conf(topology: &TopologyConfig) -> String {
    let mut out = String::new();
    out.push_str("# Managed by nullroute — do not edit manually\n");
    out.push_str("context.objects = [\n");

    for master in &topology.masters {
        out.push_str("    { factory = adapter\n");
        out.push_str("        args = {\n");
        out.push_str("            factory.name     = support.null-audio-sink\n");
        out.push_str(&format!(
            "            node.name        = \"{}\"\n",
            escape_str(&master.name)
        ));
        if let Some(desc) = &master.description {
            if !desc.is_empty() && desc != &master.name {
                out.push_str(&format!(
                    "            node.description = \"{}\"\n",
                    escape_str(desc)
                ));
            }
        }
        out.push_str("            media.class      = \"Audio/Duplex\"\n");
        out.push_str(&format!(
            "            audio.position   = \"{}\"\n",
            master.channel_map.as_pw_str()
        ));
        out.push_str("            monitor.channel-volumes = true\n");
        if let Some(obj_id) = master.object_id {
            out.push_str(&format!("            object.id = {}\n", obj_id));
        }
        out.push_str("        }\n");
        out.push_str("    }\n");
    }

    for slave in &topology.slaves {
        out.push_str("    { factory = adapter\n");
        out.push_str("        args = {\n");
        out.push_str("            factory.name     = support.null-audio-sink\n");
        out.push_str(&format!(
            "            node.name        = \"{}\"\n",
            escape_str(&slave.name)
        ));
        if let Some(desc) = &slave.description {
            if !desc.is_empty() && desc != &slave.name {
                out.push_str(&format!(
                    "            node.description = \"{}\"\n",
                    escape_str(desc)
                ));
            }
        }
        out.push_str("            media.class      = \"Audio/Duplex\"\n");
        out.push_str(&format!(
            "            audio.position   = \"{}\"\n",
            slave.channel_map.as_pw_str()
        ));
        out.push_str("            monitor.channel-volumes = true\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
    }

    out.push_str("]\n");
    out
}

/// Generates the `stream.rules` configuration file for a specific Slave sink.
pub fn generate_slave_rule_conf(topology: &TopologyConfig, slave: &SlaveSink) -> Option<String> {
    let slave_rules = topology.rules_for_slave(&slave.name);
    let mut all_clauses: Vec<MatchClause> = Vec::new();
    let mut set_description: Option<String> = None;

    if slave.is_catch_all {
        let auto_negated = topology.compute_catch_all_clause(&slave.name);
        if !auto_negated.is_empty() {
            all_clauses.push(auto_negated);
        }
    }

    for rule in slave_rules {
        if set_description.is_none() && rule.set_description.is_some() {
            set_description = rule.set_description.clone();
        }
        for c in &rule.matches {
            if !c.is_empty() && !all_clauses.contains(c) {
                all_clauses.push(c.clone());
            }
        }
    }

    if all_clauses.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str(&format!(
        "# Managed by nullroute — routing rules for slave '{}'\n",
        slave.name
    ));
    if slave.is_catch_all {
        out.push_str("# Includes auto-generated catch-all inverse matches\n");
    }
    out.push_str("stream.rules = [\n");
    out.push_str("  {\n");
    out.push_str("    matches = [\n");

    for clause in &all_clauses {
        out.push_str("      {\n");
        for (key, vals) in &clause.conditions {
            for val in vals {
                out.push_str(&format!(
                    "        {} = \"{}\"\n",
                    key,
                    escape_str(val)
                ));
            }
        }
        out.push_str("      }\n");
    }

    out.push_str("    ]\n");
    out.push_str("    actions = {\n");
    out.push_str("      update-props = {\n");
    if let Some(desc) = set_description {
        out.push_str(&format!(
            "        node.description = \"{}\"\n",
            escape_str(&desc)
        ));
    } else if slave.is_catch_all {
        out.push_str(&format!(
            "        node.description = \"{}\"\n",
            escape_str(&slave.name.to_ascii_lowercase())
        ));
    }
    out.push_str(&format!(
        "        target.object = \"{}\"\n",
        escape_str(&slave.name)
    ));
    out.push_str("      }\n");
    out.push_str("    }\n");
    out.push_str("  }\n");
    out.push_str("]\n");

    Some(out)
}

/// Maps a Slave sink name to its conventional `.conf` filename (matching user conventions like `browsers.conf` for `Browser`).
pub fn rule_filename_for_slave(slave_name: &str) -> String {
    let lower = slave_name.trim().to_ascii_lowercase();
    if lower == "browser" {
        "browsers.conf".to_string()
    } else {
        format!("{}.conf", lower)
    }
}

/// Writes `10-sinks.conf` and all slave rule `.conf` files into the user's PipeWire config directories.
pub fn write_configs_to_disk(
    topology: &TopologyConfig,
    user_config_dir: &Path,
    backup: bool,
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let pw_dir = user_config_dir.join("pipewire");
    let pw_conf_d = pw_dir.join("pipewire.conf.d");
    let pulse_conf_d = pw_dir.join("pipewire-pulse.conf.d");
    let client_conf_d = pw_dir.join("client.conf.d");

    for dir in [&pw_conf_d, &pulse_conf_d, &client_conf_d] {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory {}", dir.display()))?;
    }

    let backup_dir = user_config_dir.join("nullroute").join("backup");
    if backup {
        fs::create_dir_all(&backup_dir)?;
    }

    // 1. Write `10-sinks.conf` into `pipewire.conf.d`
    let sinks_path = pw_conf_d.join("10-sinks.conf");
    if backup && sinks_path.exists() {
        let _ = fs::copy(&sinks_path, backup_dir.join("10-sinks.conf.bak"));
    }
    let sinks_content = generate_sinks_conf(topology);
    fs::write(&sinks_path, sinks_content)
        .with_context(|| format!("Failed to write {}", sinks_path.display()))?;
    written.push(sinks_path);

    // 2. Write slave rule `.conf` files into `pipewire.conf.d`, `pipewire-pulse.conf.d`, and `client.conf.d`
    for slave in &topology.slaves {
        let Some(rule_content) = generate_slave_rule_conf(topology, slave) else {
            continue;
        };
        let filename = rule_filename_for_slave(&slave.name);
        for target_dir in [&pw_conf_d, &pulse_conf_d, &client_conf_d] {
            let file_path = target_dir.join(&filename);
            if backup && file_path.exists() {
                let _ = fs::copy(&file_path, backup_dir.join(format!("{}.bak", filename)));
            }
            // If file is a hardlink, remove before writing so we write cleanly or replace in all 3 dirs
            let _ = fs::remove_file(&file_path);
            fs::write(&file_path, &rule_content)
                .with_context(|| format!("Failed to write {}", file_path.display()))?;
            written.push(file_path);
        }
    }

    Ok(written)
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChannelMap, MasterSink, RoutingRule, SlaveSink};
    use crate::spa_json::parse_spa_conf;

    #[test]
    fn test_generate_and_reparse_configs() {
        let mut topo = TopologyConfig::default();
        topo.masters.push(MasterSink {
            name: "Master".into(),
            description: None,
            object_id: Some(10000),
            channel_map: ChannelMap::Stereo,
            hardware_targets: vec!["alsa_output.pci-0000_37_00.6.analog-stereo".into()],
        });
        topo.slaves.push(SlaveSink {
            name: "Games".into(),
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

        let mut game_clause = MatchClause::new();
        game_clause.add_condition("application.process.binary", "factorio");
        topo.rules.push(RoutingRule {
            id: "games".into(),
            target_slave: "Games".into(),
            set_description: Some("game".into()),
            matches: vec![game_clause],
        });

        let sinks_conf = generate_sinks_conf(&topo);
        assert!(parse_spa_conf(&sinks_conf).is_ok());

        let others_conf = generate_slave_rule_conf(&topo, &topo.slaves[1]).unwrap();
        assert!(others_conf.contains("!factorio"));
        assert!(others_conf.contains("!game"));
        assert!(parse_spa_conf(&others_conf).is_ok());
    }
}
