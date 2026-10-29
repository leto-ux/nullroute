use crate::model::{normalize_sink_name, TopologyConfig};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LiveGraph {
    pub nodes: BTreeMap<u32, LiveNode>,
    pub ports: BTreeMap<u32, LivePort>,
    pub links: Vec<LiveLink>,
    pub default_audio_sink: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveNode {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub media_class: String,
    pub factory_name: Option<String>,
    pub object_serial: Option<u64>,
    pub props: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivePort {
    pub id: u32,
    pub node_id: u32,
    pub name: String,
    pub direction: String, // "in" or "out"
    pub channel: String,   // e.g. "FL", "FR", "MONO"
    pub is_monitor: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveLink {
    pub id: u32,
    pub output_node_id: u32,
    pub output_port_id: u32,
    pub input_node_id: u32,
    pub input_port_id: u32,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    Linked,
    Partial { linked: usize, total: usize },
    Unlinked,
    TargetOffline,
}

#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub created_sinks: Vec<String>,
    pub created_links: Vec<String>,
    pub removed_links: Vec<String>,
    pub rerouted_streams: Vec<String>,
    pub default_sink_updated: Option<String>,
}

impl LiveGraph {
    /// Captures a live snapshot of the PipeWire graph by invoking `pw-dump`.
    pub fn snapshot() -> Result<Self> {
        let output = Command::new("pw-dump")
            .output()
            .context("Failed to execute `pw-dump`. Is PipeWire running?")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("`pw-dump` exited with status {}: {}", output.status, stderr);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Self::from_pw_dump_json(&stdout)
    }

    /// Parses the JSON array produced by `pw-dump`.
    pub fn from_pw_dump_json(json_str: &str) -> Result<Self> {
        let items: Vec<Value> =
            serde_json::from_str(json_str).context("Failed to parse `pw-dump` JSON output")?;
        let mut graph = LiveGraph::default();

        for item in items {
            let Some(id) = item.get("id").and_then(|v| v.as_u64()).map(|n| n as u32) else {
                continue;
            };
            let Some(obj_type) = item.get("type").and_then(|v| v.as_str()) else {
                continue;
            };
            let info = item.get("info");

            match obj_type {
                "PipeWire:Interface:Node" => {
                    let Some(props_obj) = info
                        .and_then(|i| i.get("props"))
                        .and_then(|p| p.as_object())
                    else {
                        continue;
                    };
                    let mut props = BTreeMap::new();
                    for (k, v) in props_obj {
                        let val_str = match v {
                            Value::String(s) => s.clone(),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            _ => continue,
                        };
                        props.insert(k.clone(), val_str);
                    }
                    let name = props.get("node.name").cloned().unwrap_or_default();
                    let description = props
                        .get("node.description")
                        .or_else(|| props.get("node.nick"))
                        .or_else(|| props.get("application.name"))
                        .cloned()
                        .unwrap_or_else(|| name.clone());
                    let media_class = props.get("media.class").cloned().unwrap_or_default();
                    let factory_name = props.get("factory.name").cloned();
                    let object_serial = props
                        .get("object.serial")
                        .and_then(|s| s.parse::<u64>().ok());

                    graph.nodes.insert(
                        id,
                        LiveNode {
                            id,
                            name,
                            description,
                            media_class,
                            factory_name,
                            object_serial,
                            props,
                        },
                    );
                }
                "PipeWire:Interface:Port" => {
                    let Some(props_obj) = info
                        .and_then(|i| i.get("props"))
                        .and_then(|p| p.as_object())
                    else {
                        continue;
                    };
                    let node_id = props_obj
                        .get("node.id")
                        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
                        .unwrap_or(0) as u32;
                    let name = props_obj
                        .get("port.name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let raw_dir = props_obj
                        .get("port.direction")
                        .and_then(|v| v.as_str())
                        .or_else(|| info.and_then(|i| i.get("direction")).and_then(|v| v.as_str()))
                        .unwrap_or("");
                    let direction = match raw_dir {
                        "in" | "input" => "in".to_string(),
                        "out" | "output" => "out".to_string(),
                        other => other.to_string(),
                    };
                    let channel = props_obj
                        .get("audio.channel")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let is_monitor = props_obj
                        .get("port.monitor")
                        .and_then(|v| {
                            v.as_bool()
                                .or_else(|| Some(v.as_str()? == "true"))
                        })
                        .unwrap_or(false);

                    graph.ports.insert(
                        id,
                        LivePort {
                            id,
                            node_id,
                            name,
                            direction,
                            channel,
                            is_monitor,
                        },
                    );
                }
                "PipeWire:Interface:Link" => {
                    let Some(info_obj) = info.and_then(|i| i.as_object()) else {
                        continue;
                    };
                    let output_node_id = info_obj
                        .get("output-node-id")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let output_port_id = info_obj
                        .get("output-port-id")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let input_node_id = info_obj
                        .get("input-node-id")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let input_port_id = info_obj
                        .get("input-port-id")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let state = info_obj
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    graph.links.push(LiveLink {
                        id,
                        output_node_id,
                        output_port_id,
                        input_node_id,
                        input_port_id,
                        state,
                    });
                }
                "PipeWire:Interface:Metadata" => {
                    let is_default_meta = item
                        .get("props")
                        .and_then(|p| p.get("metadata.name"))
                        .and_then(|v| v.as_str())
                        == Some("default");
                    if is_default_meta {
                        if let Some(meta_arr) = item.get("metadata").and_then(|m| m.as_array()) {
                            for entry in meta_arr {
                                let key = entry.get("key").and_then(|v| v.as_str());
                                if matches!(
                                    key,
                                    Some("default.audio.sink")
                                        | Some("default.configured.audio.sink")
                                ) {
                                    if let Some(name) = entry
                                        .get("value")
                                        .and_then(|v| v.get("name"))
                                        .and_then(|n| n.as_str())
                                    {
                                        graph.default_audio_sink = Some(name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(graph)
    }

    /// Returns all physical hardware audio output sinks (`Audio/Sink` excluding null/virtual sinks).
    pub fn hardware_sinks(&self) -> Vec<&LiveNode> {
        self.nodes
            .values()
            .filter(|n| {
                n.media_class == "Audio/Sink"
                    && n.factory_name.as_deref() != Some("support.null-audio-sink")
                    && !n.props.contains_key("virtual.sink")
            })
            .collect()
    }

    /// Returns all virtual sinks (`Audio/Duplex` or `support.null-audio-sink`).
    pub fn virtual_sinks(&self) -> Vec<&LiveNode> {
        self.nodes
            .values()
            .filter(|n| {
                n.media_class == "Audio/Duplex"
                    || (n.media_class == "Audio/Sink"
                        && n.factory_name.as_deref() == Some("support.null-audio-sink"))
            })
            .collect()
    }

    /// Returns all active application audio playback streams (`Stream/Output/Audio`).
    pub fn playback_streams(&self) -> Vec<&LiveNode> {
        self.nodes
            .values()
            .filter(|n| {
                n.media_class == "Stream/Output/Audio"
                    && !n.name.starts_with("output.nullroute_")
                    && !n.name.starts_with("input.nullroute_")
            })
            .collect()
    }

    /// Finds a virtual sink by exact `node.name` or normalized name (e.g. `"Master"` matches `"1. Master"`).
    pub fn find_virtual_sink(&self, name: &str) -> Option<&LiveNode> {
        let norm = normalize_sink_name(name);
        self.virtual_sinks()
            .into_iter()
            .find(|n| n.name == name || normalize_sink_name(&n.name).eq_ignore_ascii_case(&norm))
    }

    /// Finds a hardware sink by exact `node.name` or description substring.
    pub fn find_hardware_sink(&self, target: &str) -> Option<&LiveNode> {
        self.hardware_sinks().into_iter().find(|n| {
            n.name == target
                || n.description.eq_ignore_ascii_case(target)
                || n.name.contains(target)
        })
    }

    /// Finds an output (capture/monitor) port on `node_id` for the given `channel` (e.g. `"FL"`).
    pub fn find_output_port(&self, node_id: u32, channel: &str) -> Option<&LivePort> {
        let expected_capture = format!("capture_{}", channel);
        let expected_output = format!("output_{}", channel);
        let expected_monitor = format!("monitor_{}", channel);
        self.ports.values().find(|p| {
            p.node_id == node_id
                && p.direction == "out"
                && (p.channel.eq_ignore_ascii_case(channel)
                    || p.name.eq_ignore_ascii_case(&expected_capture)
                    || p.name.eq_ignore_ascii_case(&expected_output)
                    || p.name.eq_ignore_ascii_case(&expected_monitor))
        })
    }

    /// Finds an input (playback) port on `node_id` for the given `channel` (e.g. `"FL"`).
    pub fn find_input_port(&self, node_id: u32, channel: &str) -> Option<&LivePort> {
        let expected_playback = format!("playback_{}", channel);
        let expected_input = format!("input_{}", channel);
        self.ports.values().find(|p| {
            p.node_id == node_id
                && p.direction == "in"
                && (p.channel.eq_ignore_ascii_case(channel)
                    || p.name.eq_ignore_ascii_case(&expected_playback)
                    || p.name.eq_ignore_ascii_case(&expected_input))
        })
    }

    /// Checks whether `src_node_id` (output/capture ports) is linked to `dst_node_id` (input/playback ports)
    /// across all requested `channels`.
    pub fn check_link_status(
        &self,
        src_node_id: u32,
        dst_node_id: u32,
        channels: &[&str],
    ) -> LinkStatus {
        let mut linked = 0;
        let total = channels.len();
        for ch in channels {
            let Some(out_port) = self.find_output_port(src_node_id, ch) else {
                continue;
            };
            let Some(in_port) = self.find_input_port(dst_node_id, ch) else {
                continue;
            };
            let exists = self
                .links
                .iter()
                .any(|l| l.output_port_id == out_port.id && l.input_port_id == in_port.id);
            if exists {
                linked += 1;
            }
        }
        if linked == 0 {
            LinkStatus::Unlinked
        } else if linked == total {
            LinkStatus::Linked
        } else {
            LinkStatus::Partial { linked, total }
        }
    }

    /// Returns the downstream nodes that `src_node_id`'s output ports are currently linked to.
    pub fn downstream_nodes(&self, src_node_id: u32) -> Vec<&LiveNode> {
        let mut dst_ids: Vec<u32> = self
            .links
            .iter()
            .filter(|l| l.output_node_id == src_node_id)
            .map(|l| l.input_node_id)
            .collect();
        dst_ids.sort_unstable();
        dst_ids.dedup();
        dst_ids
            .into_iter()
            .filter_map(|id| self.nodes.get(&id))
            .collect()
    }

    /// Returns the upstream playback streams (`Stream/Output/Audio`) currently feeding `sink_node_id`.
    pub fn streams_feeding_sink(&self, sink_node_id: u32) -> Vec<&LiveNode> {
        let mut src_ids: Vec<u32> = self
            .links
            .iter()
            .filter(|l| l.input_node_id == sink_node_id)
            .map(|l| l.output_node_id)
            .collect();
        src_ids.sort_unstable();
        src_ids.dedup();
        src_ids
            .into_iter()
            .filter_map(|id| self.nodes.get(&id))
            .filter(|n| n.media_class == "Stream/Output/Audio")
            .collect()
    }
}

/// Synchronizes the live PipeWire graph with the desired `TopologyConfig` without restarting PipeWire
/// and without modifying any hardware device volumes.
pub fn sync_live_graph(topology: &TopologyConfig, dry_run: bool) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let mut graph = LiveGraph::snapshot()?;

    // 1. Ensure all Master and Slave virtual sinks exist in the live session
    let mut created_any_sink = false;
    for master in &topology.masters {
        if graph.find_virtual_sink(&master.name).is_none() {
            report.created_sinks.push(master.name.clone());
            if !dry_run {
                create_live_null_sink(&master.name, master.channel_map.as_pw_str())?;
                created_any_sink = true;
            }
        }
    }
    for slave in &topology.slaves {
        if graph.find_virtual_sink(&slave.name).is_none() {
            report.created_sinks.push(slave.name.clone());
            if !dry_run {
                create_live_null_sink(&slave.name, slave.channel_map.as_pw_str())?;
                created_any_sink = true;
            }
        }
    }

    if created_any_sink && !dry_run {
        // Refresh snapshot so new node & port IDs are visible
        std::thread::sleep(std::time::Duration::from_millis(120));
        graph = LiveGraph::snapshot()?;
    }

    // 2. Link Master capture ports -> Hardware playback ports
    let mut desired_port_links: Vec<(u32, u32)> = Vec::new();

    for master in &topology.masters {
        let Some(master_node) = graph.find_virtual_sink(&master.name) else {
            continue;
        };
        for hw_target in &master.hardware_targets {
            let Some(hw_node) = graph.find_hardware_sink(hw_target) else {
                continue;
            };
            for ch in master.channel_map.channels() {
                if let (Some(out_p), Some(in_p)) = (
                    graph.find_output_port(master_node.id, ch),
                    graph.find_input_port(hw_node.id, ch),
                ) {
                    desired_port_links.push((out_p.id, in_p.id));
                    let already_linked = graph
                        .links
                        .iter()
                        .any(|l| l.output_port_id == out_p.id && l.input_port_id == in_p.id);
                    if !already_linked {
                        report.created_links.push(format!(
                            "{}:{} -> {}:{}",
                            master_node.name, out_p.name, hw_node.name, in_p.name
                        ));
                        if !dry_run {
                            pw_link_ports(out_p.id, in_p.id)?;
                        }
                    }
                }
            }
        }
    }

    // 3. Link Slave capture ports -> Master playback ports
    for slave in &topology.slaves {
        let Some(slave_node) = graph.find_virtual_sink(&slave.name) else {
            continue;
        };
        for master_name in &slave.masters {
            let Some(master_node) = graph.find_virtual_sink(master_name) else {
                continue;
            };
            for ch in slave.channel_map.channels() {
                if let (Some(out_p), Some(in_p)) = (
                    graph.find_output_port(slave_node.id, ch),
                    graph.find_input_port(master_node.id, ch),
                ) {
                    desired_port_links.push((out_p.id, in_p.id));
                    let already_linked = graph
                        .links
                        .iter()
                        .any(|l| l.output_port_id == out_p.id && l.input_port_id == in_p.id);
                    if !already_linked {
                        report.created_links.push(format!(
                            "{}:{} -> {}:{}",
                            slave_node.name, out_p.name, master_node.name, in_p.name
                        ));
                        if !dry_run {
                            pw_link_ports(out_p.id, in_p.id)?;
                        }
                    }
                }
            }
        }
    }

    // 4. Remove stale links outgoing from managed Master/Slave capture ports that are not in `desired_port_links`
    let managed_virtual_ids: Vec<u32> = topology
        .masters
        .iter()
        .filter_map(|m| graph.find_virtual_sink(&m.name).map(|n| n.id))
        .chain(
            topology
                .slaves
                .iter()
                .filter_map(|s| graph.find_virtual_sink(&s.name).map(|n| n.id)),
        )
        .collect();

    for link in &graph.links {
        if managed_virtual_ids.contains(&link.output_node_id)
            && !desired_port_links.contains(&(link.output_port_id, link.input_port_id))
        {
            let src_name = graph
                .nodes
                .get(&link.output_node_id)
                .map(|n| n.name.as_str())
                .unwrap_or("?");
            let dst_name = graph
                .nodes
                .get(&link.input_node_id)
                .map(|n| n.name.as_str())
                .unwrap_or("?");
            report
                .removed_links
                .push(format!("{} -> {} (link #{})", src_name, dst_name, link.id));
            if !dry_run {
                let _ = Command::new("pw-link")
                    .arg("-d")
                    .arg(link.id.to_string())
                    .output();
            }
        }
    }

    // 5. Update default sink if a Slave is marked `is_default_sink = true`
    if let Some(default_slave) = topology.slaves.iter().find(|s| s.is_default_sink) {
        if let Some(slave_node) = graph.find_virtual_sink(&default_slave.name) {
            if graph.default_audio_sink.as_deref() != Some(slave_node.name.as_str()) {
                report.default_sink_updated = Some(slave_node.name.clone());
                if !dry_run {
                    let _ = Command::new("wpctl")
                        .arg("set-default")
                        .arg(slave_node.id.to_string())
                        .output();
                }
            }
        }
    }

    // 6. Re-route active application streams to their matched Slave sink
    for stream in graph.playback_streams() {
        let Some(target_slave_name) = topology.match_stream_target(&stream.props) else {
            continue;
        };
        let Some(target_slave_node) = graph.find_virtual_sink(&target_slave_name) else {
            continue;
        };
        let current_targets = graph.downstream_nodes(stream.id);
        let already_on_target = current_targets.len() == 1
            && current_targets[0].id == target_slave_node.id;

        if !already_on_target {
            let current_desc = if current_targets.is_empty() {
                "unlinked".to_string()
            } else {
                current_targets
                    .iter()
                    .map(|n| n.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            report.rerouted_streams.push(format!(
                "{} (id:{}) : {} -> {}",
                stream.description, stream.id, current_desc, target_slave_node.name
            ));

            if !dry_run {
                // Update WirePlumber stream target metadata
                if let Some(serial) = target_slave_node.object_serial {
                    let _ = Command::new("pw-metadata")
                        .args([
                            "-n",
                            "default",
                            &stream.id.to_string(),
                            "target.object",
                            &serial.to_string(),
                            "Spa:Id",
                        ])
                        .output();
                }
                // Also directly re-link FL/FR ports for immediate effect (especially for PipeWire ALSA streams)
                for ch in ["FL", "FR"] {
                    if let (Some(stream_out), Some(slave_in)) = (
                        graph.find_output_port(stream.id, ch),
                        graph.find_input_port(target_slave_node.id, ch),
                    ) {
                        let _ = pw_link_ports(stream_out.id, slave_in.id);
                        for old_link in graph
                            .links
                            .iter()
                            .filter(|l| l.output_port_id == stream_out.id && l.input_port_id != slave_in.id)
                        {
                            let _ = Command::new("pw-link")
                                .arg("-d")
                                .arg(old_link.id.to_string())
                                .output();
                        }
                    }
                }
            }
        }
    }

    Ok(report)
}

fn create_live_null_sink(name: &str, audio_position: &str) -> Result<()> {
    let props = format!(
        "{{ factory.name = support.null-audio-sink node.name = \"{}\" media.class = \"Audio/Duplex\" audio.position = \"{}\" monitor.channel-volumes = true }}",
        name, audio_position
    );
    let output = Command::new("pw-cli")
        .args(["create-node", "adapter", &props])
        .output()
        .context("Failed to execute `pw-cli create-node`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create virtual sink '{}': {}", name, stderr);
    }
    Ok(())
}

fn pw_link_ports(out_port_id: u32, in_port_id: u32) -> Result<()> {
    let output = Command::new("pw-link")
        .arg(out_port_id.to_string())
        .arg(in_port_id.to_string())
        .output()
        .context("Failed to execute `pw-link`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("File exists") {
            bail!(
                "Failed to link port {} -> {}: {}",
                out_port_id,
                in_port_id,
                stderr.trim()
            );
        }
    }
    Ok(())
}
