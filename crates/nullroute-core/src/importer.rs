use crate::model::{
    normalize_sink_name, ChannelMap, MasterSink, MatchClause, RoutingRule, SlaveSink,
    TopologyConfig,
};
use crate::runtime::LiveGraph;
use crate::spa_json::{parse_spa_conf, SpaValue};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    pub files_parsed: Vec<String>,
    pub masters_imported: Vec<String>,
    pub slaves_imported: Vec<String>,
    pub rules_imported: usize,
    pub hardware_links_imported: usize,
}

/// Imports virtual sinks and stream routing rules from `~/.config/pipewire` and `~/.config/wireplumber`.
pub fn import_from_conf_root(
    user_config_dir: &Path,
    topology: &mut TopologyConfig,
) -> Result<ImportSummary> {
    let mut summary = ImportSummary::default();
    let subdirs = [
        user_config_dir.join("pipewire").join("pipewire.conf.d"),
        user_config_dir
            .join("pipewire")
            .join("pipewire-pulse.conf.d"),
        user_config_dir.join("pipewire").join("client.conf.d"),
        user_config_dir
            .join("wireplumber")
            .join("wireplumber.conf.d"),
    ];

    let mut discovered_null_sinks: Vec<(String, Option<String>, Option<u32>, ChannelMap)> =
        Vec::new();

    for dir in &subdirs {
        if !dir.exists() {
            continue;
        }
        let mut entries: Vec<_> = fs::read_dir(dir)
            .with_context(|| format!("Failed to read directory {}", dir.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("conf"))
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            let Ok(spa_root) = parse_spa_conf(&content) else {
                continue;
            };
            summary.files_parsed.push(path.display().to_string());

            // 1. Check `context.objects` for `support.null-audio-sink`
            if let Some(objs) = spa_root
                .get_field("context.objects")
                .and_then(|v| v.as_array())
            {
                for obj in objs {
                    let Some(args) = obj.get_field("args") else {
                        continue;
                    };
                    let factory_name = args.get_field("factory.name").and_then(|v| v.as_str());
                    if factory_name != Some("support.null-audio-sink") {
                        continue;
                    }
                    let Some(raw_name) = args.get_field("node.name").and_then(|v| v.as_str())
                    else {
                        continue;
                    };
                    let name = normalize_sink_name(raw_name);
                    let description = args
                        .get_field("node.description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let object_id = args.get_field("object.id").and_then(|v| v.as_u32());
                    let channel_map = args
                        .get_field("audio.position")
                        .and_then(|v| v.as_str())
                        .map(ChannelMap::from_pw_str)
                        .unwrap_or_default();

                    if !discovered_null_sinks
                        .iter()
                        .any(|(n, _, _, _)| n.eq_ignore_ascii_case(&name))
                    {
                        discovered_null_sinks.push((name, description, object_id, channel_map));
                    }
                }
            }

            // 2. Check `stream.rules`
            if let Some(rules_arr) = spa_root
                .get_field("stream.rules")
                .and_then(|v| v.as_array())
            {
                import_stream_rules_array(rules_arr, topology, &mut summary);
            }
        }
    }

    // Classify discovered null sinks into Masters vs Slaves first (preserving `10-sinks.conf` order)
    let rule_targets: BTreeSet<String> = topology
        .rules
        .iter()
        .map(|r| normalize_sink_name(&r.target_slave).to_ascii_lowercase())
        .collect();

    let mut ordered_slaves = Vec::new();
    for (name, description, object_id, channel_map) in discovered_null_sinks {
        let lower = name.to_ascii_lowercase();
        let is_master = (lower.contains("master") || object_id == Some(10000))
            && !rule_targets.contains(&lower);

        if is_master {
            if topology.find_master(&name).is_none() {
                topology.masters.push(MasterSink {
                    name: name.clone(),
                    description,
                    object_id,
                    channel_map,
                    hardware_targets: Vec::new(),
                });
                summary.masters_imported.push(name);
            }
        } else {
            let existing = topology.find_slave(&name).cloned();
            let is_catch_all = existing.as_ref().map(|s| s.is_catch_all).unwrap_or(false)
                || lower == "others"
                || lower == "fallback";
            let is_default_sink = existing
                .as_ref()
                .map(|s| s.is_default_sink)
                .unwrap_or(false);
            let masters = existing.map(|s| s.masters).unwrap_or_default();

            ordered_slaves.push(SlaveSink {
                name: name.clone(),
                description,
                channel_map,
                masters,
                is_default_sink,
                is_catch_all,
            });
            if !summary
                .slaves_imported
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&name))
            {
                summary.slaves_imported.push(name);
            }
        }
    }

    // Append any rule-only slaves that weren't in `context.objects`
    for existing_slave in std::mem::take(&mut topology.slaves) {
        if !ordered_slaves.iter().any(|s| {
            normalize_sink_name(&s.name)
                .eq_ignore_ascii_case(&normalize_sink_name(&existing_slave.name))
        }) {
            ordered_slaves.push(existing_slave);
        }
    }
    topology.slaves = ordered_slaves;

    // If Slaves have no master linked yet and at least one Master exists, link them to the primary Master
    if let Some(first_master) = topology.masters.first().map(|m| m.name.clone()) {
        for slave in &mut topology.slaves {
            if slave.masters.is_empty() {
                slave.masters.push(first_master.clone());
            }
        }
    }

    Ok(summary)
}

fn import_stream_rules_array(
    rules_arr: &[SpaValue],
    topology: &mut TopologyConfig,
    summary: &mut ImportSummary,
) {
    for rule_obj in rules_arr {
        let Some(update_props) = rule_obj
            .get_field("actions")
            .and_then(|a| a.get_field("update-props"))
        else {
            continue;
        };
        let Some(raw_target) = update_props
            .get_field("target.object")
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        let target_slave = normalize_sink_name(raw_target);
        let set_description = update_props
            .get_field("node.description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let Some(matches_arr) = rule_obj.get_field("matches").and_then(|m| m.as_array()) else {
            continue;
        };

        let mut clauses = Vec::new();
        let mut saw_catch_all_negation = false;

        for match_item in matches_arr {
            let Some(entries) = match_item.as_object() else {
                continue;
            };
            let mut clause = MatchClause::new();
            for (k, v) in entries {
                if let Some(val_str) = v.as_str() {
                    clause.add_condition(k.clone(), val_str.to_string());
                }
            }
            if clause.is_predominantly_negated() {
                saw_catch_all_negation = true;
            } else if !clause.is_empty() {
                clauses.push(clause);
            }
        }

        if saw_catch_all_negation {
            if let Some(slave) = topology.find_slave_mut(&target_slave) {
                slave.is_catch_all = true;
            } else {
                topology.slaves.push(SlaveSink {
                    name: target_slave.clone(),
                    description: None,
                    channel_map: ChannelMap::Stereo,
                    masters: Vec::new(),
                    is_default_sink: false,
                    is_catch_all: true,
                });
                summary.slaves_imported.push(target_slave.clone());
            }
        }

        if clauses.is_empty() && !saw_catch_all_negation {
            continue;
        }

        // Check if an identical or existing rule for this target_slave already exists (due to hardlinked conf files)
        if let Some(existing_rule) = topology.rules.iter_mut().find(|r| {
            normalize_sink_name(&r.target_slave).eq_ignore_ascii_case(&target_slave)
        }) {
            if existing_rule.set_description.is_none() && set_description.is_some() {
                existing_rule.set_description = set_description;
            }
            for c in clauses {
                if !existing_rule.matches.contains(&c) {
                    existing_rule.matches.push(c);
                }
            }
        } else {
            let rule_id = target_slave.to_ascii_lowercase();
            topology.rules.push(RoutingRule {
                id: rule_id,
                target_slave,
                set_description,
                matches: clauses,
            });
            summary.rules_imported += 1;
        }
    }
}

/// Imports and reconciles virtual sinks, Master->Hardware links, and Slave->Master links from a live PipeWire graph.
pub fn import_from_live_graph(
    graph: &LiveGraph,
    topology: &mut TopologyConfig,
) -> Result<ImportSummary> {
    let mut summary = ImportSummary::default();
    let hw_ids: BTreeSet<u32> = graph.hardware_sinks().into_iter().map(|n| n.id).collect();
    let vsinks = graph.virtual_sinks();

    // First pass: identify which virtual sinks link directly to hardware outputs -> Masters
    for vsink in &vsinks {
        let norm_name = normalize_sink_name(&vsink.name);
        let downstream = graph.downstream_nodes(vsink.id);
        let linked_hw: Vec<String> = downstream
            .iter()
            .filter(|n| hw_ids.contains(&n.id))
            .map(|n| n.name.clone())
            .collect();

        if !linked_hw.is_empty() || norm_name.eq_ignore_ascii_case("Master") {
            // If it was mistakenly in slaves, remove it from slaves
            topology
                .slaves
                .retain(|s| !normalize_sink_name(&s.name).eq_ignore_ascii_case(&norm_name));

            if let Some(master) = topology.find_master_mut(&norm_name) {
                for hw in linked_hw {
                    if !master.hardware_targets.contains(&hw) {
                        master.hardware_targets.push(hw);
                        summary.hardware_links_imported += 1;
                    }
                }
            } else {
                summary.hardware_links_imported += linked_hw.len();
                topology.masters.push(MasterSink {
                    name: norm_name.clone(),
                    description: Some(vsink.description.clone()),
                    object_id: None,
                    channel_map: ChannelMap::Stereo,
                    hardware_targets: linked_hw,
                });
                summary.masters_imported.push(norm_name);
            }
        }
    }

    // Second pass: identify Slave virtual sinks and their links to Masters
    for vsink in &vsinks {
        let norm_name = normalize_sink_name(&vsink.name);
        if topology.find_master(&norm_name).is_some() {
            continue;
        }
        let downstream = graph.downstream_nodes(vsink.id);
        let linked_masters: Vec<String> = downstream
            .iter()
            .map(|n| normalize_sink_name(&n.name))
            .filter(|dn| topology.find_master(dn).is_some())
            .collect();

        if let Some(slave) = topology.find_slave_mut(&norm_name) {
            for m in linked_masters {
                if !slave
                    .masters
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&m))
                {
                    slave.masters.push(m);
                }
            }
        } else {
            let is_catch_all = norm_name.eq_ignore_ascii_case("Others");
            topology.slaves.push(SlaveSink {
                name: norm_name.clone(),
                description: Some(vsink.description.clone()),
                channel_map: ChannelMap::Stereo,
                masters: linked_masters,
                is_default_sink: false,
                is_catch_all,
            });
            summary.slaves_imported.push(norm_name);
        }
    }

    // Check default sink
    if let Some(def_sink) = &graph.default_audio_sink {
        let norm_def = normalize_sink_name(def_sink);
        if let Some(slave) = topology.find_slave_mut(&norm_def) {
            slave.is_default_sink = true;
        }
    }

    Ok(summary)
}
