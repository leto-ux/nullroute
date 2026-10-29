use crate::model::{normalize_sink_name, TopologyConfig};
use crate::runtime::{LinkStatus, LiveGraph};

#[derive(Debug, Clone)]
pub struct TreeOptions {
    pub show_rules: bool,
    pub show_streams: bool,
    pub flow_direction: bool,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            show_rules: true,
            show_streams: true,
            flow_direction: false,
        }
    }
}

/// Renders an ASCII/Unicode tree visualization of the audio topology and optional live PipeWire graph state.
pub fn render_topology_tree(
    topology: &TopologyConfig,
    live: Option<&LiveGraph>,
    opts: &TreeOptions,
) -> String {
    if opts.flow_direction {
        render_flow_map(topology, live, opts)
    } else {
        render_master_hierarchy(topology, live, opts)
    }
}

fn render_master_hierarchy(
    topology: &TopologyConfig,
    live: Option<&LiveGraph>,
    opts: &TreeOptions,
) -> String {
    let mut out = String::new();

    if topology.masters.is_empty() && topology.slaves.is_empty() {
        out.push_str(
            "No virtual sinks configured in nullroute yet.\nRun `nullroute import` to import existing PipeWire configs & live links, or `nullroute master add <name>`.\n",
        );
        return out;
    }

    out.push_str("nullroute Audio Routing Topology\n");
    out.push_str("================================\n");

    for (m_idx, master) in topology.masters.iter().enumerate() {
        let is_last_master = m_idx + 1 == topology.masters.len();
        let m_branch = if is_last_master { "└─" } else { "├─" };
        let m_cont = if is_last_master { "   " } else { "│  " };

        let live_master = live.and_then(|g| g.find_virtual_sink(&master.name));
        let live_badge = match (live.is_some(), live_master) {
            (false, _) => String::new(),
            (true, Some(n)) if n.name != master.name => {
                format!(" [LIVE as '{}' #{}]", n.name, n.id)
            }
            (true, Some(n)) => format!(" [LIVE #{}]", n.id),
            (true, None) => " [NOT RUNNING]".to_string(),
        };

        out.push_str(&format!(
            "{} [Master] {} ({}){}\n",
            m_branch, master.name, master.channel_map, live_badge
        ));

        // 1. Hardware Outputs section under Master
        let connected_slaves: Vec<_> = topology
            .slaves
            .iter()
            .filter(|s| {
                s.masters
                    .iter()
                    .any(|m| normalize_sink_name(m).eq_ignore_ascii_case(&master.name))
            })
            .collect();

        let direct_streams = live
            .and_then(|g| live_master.map(|lm| g.streams_feeding_sink(lm.id)))
            .unwrap_or_default();

        let has_slaves_or_streams = !connected_slaves.is_empty() || !direct_streams.is_empty();
        let hw_sec_branch = if has_slaves_or_streams {
            "├─"
        } else {
            "└─"
        };
        let hw_sec_cont = if has_slaves_or_streams {
            "│  "
        } else {
            "   "
        };

        out.push_str(&format!(
            "{}{} ──► Hardware Outputs ({} linked, volume: 100% untouched)\n",
            m_cont,
            hw_sec_branch,
            master.hardware_targets.len()
        ));

        if master.hardware_targets.is_empty() {
            out.push_str(&format!(
                "{}{}    └─ (none configured — use `nullroute master link-hw {} <device>`)\n",
                m_cont, hw_sec_cont, master.name
            ));
        } else {
            for (h_idx, hw_name) in master.hardware_targets.iter().enumerate() {
                let is_last_hw = h_idx + 1 == master.hardware_targets.len();
                let h_branch = if is_last_hw { "└─" } else { "├─" };

                let (hw_desc, link_badge) = if let Some(g) = live {
                    if let Some(hw_node) = g.find_hardware_sink(hw_name) {
                        let status = if let Some(lm) = live_master {
                            g.check_link_status(lm.id, hw_node.id, master.channel_map.channels())
                        } else {
                            LinkStatus::Unlinked
                        };
                        (
                            format!(" ({})", hw_node.description),
                            format!(" {}", format_link_status(status)),
                        )
                    } else {
                        (String::new(), " [OFFLINE]".to_string())
                    }
                } else {
                    (String::new(), String::new())
                };

                out.push_str(&format!(
                    "{}{}    {} {}{}{}\n",
                    m_cont, hw_sec_cont, h_branch, hw_name, hw_desc, link_badge
                ));
            }
        }

        // 2. Direct streams feeding Master (warn if misrouted!)
        if opts.show_streams && !direct_streams.is_empty() {
            let direct_branch = if !connected_slaves.is_empty() {
                "├─"
            } else {
                "└─"
            };
            let direct_cont = if !connected_slaves.is_empty() {
                "│  "
            } else {
                "   "
            };
            out.push_str(&format!(
                "{}{} ◄── Direct App Streams on Master (bypassing Slaves)\n",
                m_cont, direct_branch
            ));
            for (st_idx, stream) in direct_streams.iter().enumerate() {
                let is_last_st = st_idx + 1 == direct_streams.len();
                let st_branch = if is_last_st { "└─" } else { "├─" };
                let expected = topology.match_stream_target(&stream.props);
                let route_note = match expected {
                    Some(exp) => format!(" [MISROUTED: rule expects '{}']", exp),
                    None => String::new(),
                };
                let pid = stream
                    .props
                    .get("application.process.id")
                    .map(|p| format!(" pid:{}", p))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "{}{}    {} {} (node='{}'{}) {}\n",
                    m_cont, direct_cont, st_branch, stream.description, stream.name, pid, route_note
                ));
            }
        }

        // 3. Connected Slave Sinks
        if !connected_slaves.is_empty() {
            out.push_str(&format!(
                "{}└─ ◄── Slave Channels ({})\n",
                m_cont,
                connected_slaves.len()
            ));

            for (s_idx, slave) in connected_slaves.iter().enumerate() {
                let is_last_slave = s_idx + 1 == connected_slaves.len();
                let s_branch = if is_last_slave { "└─" } else { "├─" };
                let s_cont = if is_last_slave { "   " } else { "│  " };

                let mut tags = Vec::new();
                tags.push(slave.channel_map.to_string());
                if slave.is_catch_all {
                    tags.push("CATCH-ALL".to_string());
                }
                if slave.is_default_sink {
                    tags.push("DEFAULT".to_string());
                }

                let live_slave = live.and_then(|g| g.find_virtual_sink(&slave.name));
                let slave_link_badge = match (live, live_slave, live_master) {
                    (Some(g), Some(ls), Some(lm)) => {
                        let st = g.check_link_status(ls.id, lm.id, slave.channel_map.channels());
                        let name_note = if ls.name != slave.name {
                            format!(" (live: '{}')", ls.name)
                        } else {
                            String::new()
                        };
                        format!(" {}{}", format_link_status(st), name_note)
                    }
                    (Some(_), None, _) => " [NOT RUNNING]".to_string(),
                    _ => String::new(),
                };

                out.push_str(&format!(
                    "{}   {} [Slave] {} [{}]{}\n",
                    m_cont,
                    s_branch,
                    slave.name,
                    tags.join(", "),
                    slave_link_badge
                ));

                // Rules and Active Streams under this Slave
                let rules = topology.rules_for_slave(&slave.name);
                let clauses: Vec<_> = rules.iter().flat_map(|r| r.matches.iter()).collect();
                let active_streams = live
                    .and_then(|g| live_slave.map(|ls| g.streams_feeding_sink(ls.id)))
                    .unwrap_or_default();

                let show_rules_block = opts.show_rules && (!clauses.is_empty() || slave.is_catch_all);
                let show_streams_block = opts.show_streams && !active_streams.is_empty();

                if show_rules_block {
                    let r_branch = if show_streams_block { "├─" } else { "└─" };
                    let r_cont = if show_streams_block { "│  " } else { "   " };
                    out.push_str(&format!("{}   {}{} Rules:\n", m_cont, s_cont, r_branch));

                    if slave.is_catch_all {
                        let auto_clause = topology.compute_catch_all_clause(&slave.name);
                        let neg_count: usize =
                            auto_clause.conditions.values().map(|v| v.len()).sum();
                        let c_branch = if clauses.is_empty() { "└─" } else { "├─" };
                        out.push_str(&format!(
                            "{}   {}{}{} <auto-inverse catch-all: {} negated conditions>\n",
                            m_cont, s_cont, r_cont, c_branch, neg_count
                        ));
                    }

                    for (c_idx, clause) in clauses.iter().enumerate() {
                        let is_last_c = c_idx + 1 == clauses.len();
                        let c_branch = if is_last_c { "└─" } else { "├─" };
                        out.push_str(&format!(
                            "{}   {}{}{} {}\n",
                            m_cont,
                            s_cont,
                            r_cont,
                            c_branch,
                            clause.display_compact()
                        ));
                    }
                }

                if show_streams_block {
                    out.push_str(&format!(
                        "{}   {}└─ Active Apps ({}):\n",
                        m_cont,
                        s_cont,
                        active_streams.len()
                    ));
                    for (a_idx, stream) in active_streams.iter().enumerate() {
                        let is_last_a = a_idx + 1 == active_streams.len();
                        let a_branch = if is_last_a { "└─" } else { "├─" };
                        let bin = stream
                            .props
                            .get("application.process.binary")
                            .map(|b| format!(" bin={}", b))
                            .unwrap_or_default();
                        let pid = stream
                            .props
                            .get("application.process.id")
                            .map(|p| format!(" pid:{}", p))
                            .unwrap_or_default();
                        out.push_str(&format!(
                            "{}   {}   {} {} (node='{}'{}{})\n",
                            m_cont, s_cont, a_branch, stream.description, stream.name, bin, pid
                        ));
                    }
                }
            }
        }
    }

    // Any orphaned Slaves not linked to any Master
    let orphaned_slaves: Vec<_> = topology
        .slaves
        .iter()
        .filter(|s| s.masters.is_empty())
        .collect();
    if !orphaned_slaves.is_empty() {
        out.push_str("\nUnlinked Slave Sinks:\n");
        for slave in orphaned_slaves {
            out.push_str(&format!(
                "└─ [Slave] {} (not linked to any Master)\n",
                slave.name
            ));
        }
    }

    out
}

fn render_flow_map(
    topology: &TopologyConfig,
    live: Option<&LiveGraph>,
    _opts: &TreeOptions,
) -> String {
    let mut out = String::new();
    out.push_str("nullroute Signal Flow Map (Apps ──► Slaves ──► Masters ──► Hardware)\n");
    out.push_str("====================================================================\n");

    for slave in &topology.slaves {
        let rules = topology.rules_for_slave(&slave.name);
        let clause_count: usize = rules.iter().map(|r| r.matches.len()).sum();
        let rule_summary = if slave.is_catch_all {
            format!("catch-all + {} rule(s)", clause_count)
        } else {
            format!("{} rule(s)", clause_count)
        };
        let masters_str = if slave.masters.is_empty() {
            "(unlinked)".to_string()
        } else {
            slave.masters.join(", ")
        };

        let mut hw_list = Vec::new();
        for m_name in &slave.masters {
            if let Some(m) = topology.find_master(m_name) {
                for hw in &m.hardware_targets {
                    if !hw_list.contains(hw) {
                        hw_list.push(hw.clone());
                    }
                }
            }
        }
        let hw_summary = if hw_list.is_empty() {
            "(no hardware)".to_string()
        } else {
            format!("{} hw device(s)", hw_list.len())
        };

        let active_count = live
            .and_then(|g| {
                let ls = g.find_virtual_sink(&slave.name)?;
                Some(g.streams_feeding_sink(ls.id).len())
            })
            .unwrap_or(0);

        out.push_str(&format!(
            "[Apps: {}, {} active] ──► [Slave: {}] ──► [Master: {}] ──► [{}]\n",
            rule_summary, active_count, slave.name, masters_str, hw_summary
        ));
    }

    out
}

fn format_link_status(status: LinkStatus) -> String {
    match status {
        LinkStatus::Linked => "[LINKED]".to_string(),
        LinkStatus::Partial { linked, total } => format!("[PARTIAL {}/{}]", linked, total),
        LinkStatus::Unlinked => "[UNLINKED]".to_string(),
        LinkStatus::TargetOffline => "[OFFLINE]".to_string(),
    }
}
