use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use nullroute_core::{
    generator::{generate_sinks_conf, generate_slave_rule_conf, write_configs_to_disk},
    importer::{import_from_conf_root, import_from_live_graph},
    model::{
        normalize_sink_name, ChannelMap, MasterSink, MatchClause, RoutingRule, SlaveSink,
        TopologyConfig,
    },
    runtime::{sync_live_graph, LiveGraph},
    tree::{render_topology_tree, TreeOptions},
};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "nullroute",
    version,
    about = "Declarative PipeWire virtual sink (Master/Slave) & WirePlumber routing rule manager"
)]
struct Cli {
    /// Path to nullroute's topology.toml file (defaults to ~/.config/nullroute/topology.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Display a visual tree/map of how Hardware Sinks, Masters, Slaves, Rules, and Apps are connected
    #[command(visible_aliases = ["map", "status"])]
    Tree {
        /// Render only configured topology without querying the live PipeWire graph
        #[arg(long)]
        no_live: bool,

        /// Hide routing rules under each Slave sink
        #[arg(long)]
        no_rules: bool,

        /// Hide active application streams
        #[arg(long)]
        no_streams: bool,

        /// Display a horizontal signal flow map (Apps ──► Slaves ──► Masters ──► Hardware)
        #[arg(long)]
        flow: bool,
    },

    /// Import existing virtual sinks, links, and stream routing rules from PipeWire configs and/or live session
    Import {
        /// Import only from ~/.config/pipewire & ~/.config/wireplumber .conf files
        #[arg(long)]
        conf_only: bool,

        /// Import only from the currently running PipeWire graph (`pw-dump`)
        #[arg(long)]
        live_only: bool,

        /// Start from an empty topology instead of merging with existing topology.toml
        #[arg(long)]
        reset: bool,

        /// Preview the imported topology and tree without saving to disk
        #[arg(long)]
        dry_run: bool,
    },

    /// Manage Master virtual sinks and their connected hardware output devices
    Master {
        #[command(subcommand)]
        action: MasterCommand,
    },

    /// Manage Slave channel virtual sinks (e.g. Browser, Games, Music, Comms, Others)
    Slave {
        #[command(subcommand)]
        action: SlaveCommand,
    },

    /// Manage application routing rules targeting Slave sinks
    Rule {
        #[command(subcommand)]
        action: RuleCommand,
    },

    /// Write PipeWire/WirePlumber config files AND synchronize the live PipeWire session immediately
    Apply {
        /// Print generated config files and planned runtime actions without modifying disk or PipeWire
        #[arg(long)]
        dry_run: bool,

        /// Write config files only, skipping live PipeWire graph synchronization
        #[arg(long)]
        no_sync: bool,

        /// Skip backing up existing .conf files before overwriting
        #[arg(long)]
        no_backup: bool,
    },

    /// Synchronize the live PipeWire session (create missing sinks/links & reroute active streams) without writing .conf files
    Sync {
        /// Preview planned runtime actions without executing them
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
enum MasterCommand {
    /// Create a new Master virtual sink
    Add {
        /// Name of the Master sink (e.g. "Master")
        name: String,

        /// Channel layout: mono, stereo, 2.1, quad, 5.1, 7.1
        #[arg(short, long, default_value = "stereo")]
        channels: ChannelMap,

        /// Hardware output node name(s) to link this Master to
        #[arg(long = "hw")]
        hardware: Vec<String>,

        /// Optional fixed PipeWire object.id
        #[arg(long)]
        object_id: Option<u32>,
    },

    /// Remove a Master virtual sink
    Rm {
        /// Name of the Master sink to remove
        name: String,
    },

    /// List configured Master virtual sinks
    List,

    /// Connect a Master sink to a hardware audio output device (volume stays untouched)
    LinkHw {
        /// Master sink name
        master: String,
        /// Hardware sink node.name (see `nullroute master list-hw`)
        hardware: String,
    },

    /// Disconnect a Master sink from a hardware audio output device
    UnlinkHw {
        /// Master sink name
        master: String,
        /// Hardware sink node.name
        hardware: String,
    },

    /// List available physical hardware audio output sinks currently detected by PipeWire
    ListHw,
}

#[derive(Subcommand, Debug)]
enum SlaveCommand {
    /// Create a new Slave virtual sink (channel)
    Add {
        /// Name of the Slave sink (e.g. "Browser", "Games", "Music", "Comms", "Others")
        name: String,

        /// Master sink(s) to connect this Slave to (defaults to first Master if omitted)
        #[arg(short, long = "master")]
        masters: Vec<String>,

        /// Channel layout: mono, stereo, 2.1, quad, 5.1, 7.1
        #[arg(short, long, default_value = "stereo")]
        channels: ChannelMap,

        /// Mark this Slave as the catch-all sink (auto-generates inverse `!` match rules)
        #[arg(long)]
        catch_all: bool,

        /// Mark this Slave as the default system Audio/Sink
        #[arg(long)]
        default: bool,
    },

    /// Remove a Slave virtual sink and its associated routing rules
    Rm {
        /// Name of the Slave sink to remove
        name: String,
    },

    /// List configured Slave virtual sinks
    List,

    /// Link a Slave sink to a Master sink
    Link {
        /// Slave sink name
        slave: String,
        /// Master sink name
        master: String,
    },

    /// Unlink a Slave sink from a Master sink
    Unlink {
        /// Slave sink name
        slave: String,
        /// Master sink name
        master: String,
    },

    /// Mark a Slave sink as the default system Audio/Sink
    SetDefault {
        /// Slave sink name
        slave: String,
    },

    /// Configure whether a Slave sink acts as the auto-negated catch-all channel
    SetCatchAll {
        /// Slave sink name
        slave: String,
        /// Disable catch-all mode on this Slave
        #[arg(long)]
        disable: bool,
    },
}

#[derive(Subcommand, Debug)]
enum RuleCommand {
    /// Add a new application routing match clause to a Slave sink
    Add {
        /// Target Slave sink name (e.g. "Games", "Browser", "Music", "Comms")
        slave: String,

        /// Match `application.name` (e.g. "dota2", "Rematch")
        #[arg(long)]
        app: Vec<String>,

        /// Match `application.process.binary` (e.g. "factorio", "thorium", "vesktop")
        #[arg(long)]
        bin: Vec<String>,

        /// Match `node.name` (e.g. "alsa_playback.firefox", "Space Engineers")
        #[arg(long)]
        node: Vec<String>,

        /// Match `media.role` (e.g. "Music", "Game")
        #[arg(long)]
        role: Vec<String>,

        /// Exclude `application.name` (`!value`)
        #[arg(long = "not-app")]
        not_app: Vec<String>,

        /// Exclude `application.process.binary` (`!value`)
        #[arg(long = "not-bin")]
        not_bin: Vec<String>,

        /// Exclude `node.name` (`!value`)
        #[arg(long = "not-node")]
        not_node: Vec<String>,

        /// Optional `node.description` override to apply when matched (e.g. "game")
        #[arg(long)]
        description: Option<String>,
    },

    /// List configured application routing rules
    List {
        /// Filter by Slave sink name
        #[arg(short, long)]
        slave: Option<String>,
    },

    /// Remove a match clause (by 1-based index) or all rules for a Slave sink
    Rm {
        /// Target Slave sink name
        slave: String,

        /// 1-based index of the match clause to remove (see `nullroute rule list`)
        index: Option<usize>,

        /// Remove all rules for this Slave
        #[arg(long)]
        all: bool,
    },

    /// Inspect a currently running audio app (by PID, binary, or name), create a routing rule for it, and route it to a Slave
    AssignRunning {
        /// PID, binary name, application.name, or node.name of a currently active audio stream
        query: String,

        /// Target Slave sink name
        slave: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = match cli.config {
        Some(p) => p,
        None => TopologyConfig::default_path()?,
    };

    let command = cli.command.unwrap_or(Commands::Tree {
        no_live: false,
        no_rules: false,
        no_streams: false,
        flow: false,
    });

    match command {
        Commands::Tree {
            no_live,
            no_rules,
            no_streams,
            flow,
        } => {
            let mut topology = TopologyConfig::load_or_default(&config_path)?;
            let live_graph = if no_live {
                None
            } else {
                LiveGraph::snapshot().ok()
            };

            // If topology.toml hasn't been created yet, auto-preview from live + ~/.config so `nullroute tree` works out-of-the-box!
            let auto_preview = !config_path.exists()
                && topology.masters.is_empty()
                && topology.slaves.is_empty();
            if auto_preview {
                if let Some(cfg_dir) = dirs::config_dir() {
                    let _ = import_from_conf_root(&cfg_dir, &mut topology);
                }
                if let Some(ref g) = live_graph {
                    let _ = import_from_live_graph(g, &mut topology);
                }
            }

            let opts = TreeOptions {
                show_rules: !no_rules,
                show_streams: !no_streams,
                flow_direction: flow,
            };
            let rendered = render_topology_tree(&topology, live_graph.as_ref(), &opts);
            print!("{}", rendered);
            if auto_preview && (!topology.masters.is_empty() || !topology.slaves.is_empty()) {
                println!(
                    "\n(Previewing detected system setup — run `nullroute import` to save to {})",
                    config_path.display()
                );
            }
        }

        Commands::Import {
            conf_only,
            live_only,
            reset,
            dry_run,
        } => {
            let mut topology = if reset {
                TopologyConfig::default()
            } else {
                TopologyConfig::load_or_default(&config_path)?
            };

            let do_conf = !live_only || conf_only;
            let do_live = !conf_only || live_only;

            if do_conf {
                let user_cfg = dirs::config_dir()
                    .context("Could not determine user ~/.config directory")?;
                let conf_sum = import_from_conf_root(&user_cfg, &mut topology)?;
                println!(
                    "Parsed {} config file(s): found {} master(s), {} slave(s), {} rule group(s).",
                    conf_sum.files_parsed.len(),
                    conf_sum.masters_imported.len(),
                    conf_sum.slaves_imported.len(),
                    conf_sum.rules_imported
                );
            }

            let live_graph = if do_live {
                let g = LiveGraph::snapshot()?;
                let live_sum = import_from_live_graph(&g, &mut topology)?;
                println!(
                    "Inspected live PipeWire graph: imported {} hardware link(s).",
                    live_sum.hardware_links_imported
                );
                Some(g)
            } else {
                None
            };

            println!();
            let rendered =
                render_topology_tree(&topology, live_graph.as_ref(), &TreeOptions::default());
            print!("{}", rendered);

            if dry_run {
                println!("\n--- Dry-run TOML preview ---");
                println!("{}", toml::to_string_pretty(&topology)?);
            } else {
                topology.save(&config_path)?;
                println!("\nSaved imported topology to {}", config_path.display());
            }
        }

        Commands::Master { action } => {
            handle_master_cmd(action, &config_path)?;
        }

        Commands::Slave { action } => {
            handle_slave_cmd(action, &config_path)?;
        }

        Commands::Rule { action } => {
            handle_rule_cmd(action, &config_path)?;
        }

        Commands::Apply {
            dry_run,
            no_sync,
            no_backup,
        } => {
            let topology = TopologyConfig::load(&config_path).with_context(|| {
                format!(
                    "No topology config found at {}. Run `nullroute import` first.",
                    config_path.display()
                )
            })?;

            if dry_run {
                println!("=== [DRY RUN] 10-sinks.conf ===");
                print!("{}", generate_sinks_conf(&topology));
                for slave in &topology.slaves {
                    if let Some(rule_conf) = generate_slave_rule_conf(&topology, slave) {
                        println!("\n=== [DRY RUN] Rule config for '{}' ===", slave.name);
                        print!("{}", rule_conf);
                    }
                }
            } else {
                let user_cfg = dirs::config_dir()
                    .context("Could not determine user ~/.config directory")?;
                let written = write_configs_to_disk(&topology, &user_cfg, !no_backup)?;
                println!("Wrote {} configuration file(s):", written.len());
                for p in &written {
                    println!("  - {}", p.display());
                }
            }

            if !no_sync {
                let report = sync_live_graph(&topology, dry_run)?;
                print_sync_report(&report, dry_run);
            }
        }

        Commands::Sync { dry_run } => {
            let topology = TopologyConfig::load(&config_path).with_context(|| {
                format!(
                    "No topology config found at {}. Run `nullroute import` first.",
                    config_path.display()
                )
            })?;
            let report = sync_live_graph(&topology, dry_run)?;
            print_sync_report(&report, dry_run);
        }
    }

    Ok(())
}

fn handle_master_cmd(action: MasterCommand, config_path: &PathBuf) -> Result<()> {
    match action {
        MasterCommand::Add {
            name,
            channels,
            hardware,
            object_id,
        } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let norm = normalize_sink_name(&name);
            if topology.find_master(&norm).is_some() {
                bail!("Master sink '{}' already exists.", norm);
            }
            topology.masters.push(MasterSink {
                name: norm.clone(),
                description: None,
                object_id,
                channel_map: channels,
                hardware_targets: hardware,
            });
            topology.save(config_path)?;
            println!("Added Master sink '{}' ({}).", norm, channels);
        }

        MasterCommand::Rm { name } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let norm = normalize_sink_name(&name);
            let before = topology.masters.len();
            topology
                .masters
                .retain(|m| !normalize_sink_name(&m.name).eq_ignore_ascii_case(&norm));
            if topology.masters.len() == before {
                bail!("Master sink '{}' not found.", norm);
            }
            for slave in &mut topology.slaves {
                slave
                    .masters
                    .retain(|m| !normalize_sink_name(m).eq_ignore_ascii_case(&norm));
            }
            topology.save(config_path)?;
            println!("Removed Master sink '{}'.", norm);
        }

        MasterCommand::List => {
            let topology = TopologyConfig::load_or_default(config_path)?;
            if topology.masters.is_empty() {
                println!("No Master sinks configured.");
            } else {
                for m in &topology.masters {
                    println!(
                        "- {} (channels: {}, hardware targets: [{}])",
                        m.name,
                        m.channel_map,
                        m.hardware_targets.join(", ")
                    );
                }
            }
        }

        MasterCommand::LinkHw { master, hardware } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let m = topology
                .find_master_mut(&master)
                .with_context(|| format!("Master sink '{}' not found", master))?;
            if !m.hardware_targets.contains(&hardware) {
                m.hardware_targets.push(hardware.clone());
            }
            topology.save(config_path)?;
            println!("Linked Master '{}' ──► Hardware '{}'.", master, hardware);
        }

        MasterCommand::UnlinkHw { master, hardware } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let m = topology
                .find_master_mut(&master)
                .with_context(|| format!("Master sink '{}' not found", master))?;
            m.hardware_targets.retain(|h| h != &hardware);
            topology.save(config_path)?;
            println!(
                "Unlinked Master '{}' from Hardware '{}'.",
                master, hardware
            );
        }

        MasterCommand::ListHw => {
            let graph = LiveGraph::snapshot()?;
            let hw = graph.hardware_sinks();
            if hw.is_empty() {
                println!("No physical hardware audio output sinks found in PipeWire.");
            } else {
                println!("Available Hardware Audio Outputs (volumes untouched):");
                for node in hw {
                    println!(
                        "  - {} (id: #{}, desc: \"{}\")",
                        node.name, node.id, node.description
                    );
                }
            }
        }
    }
    Ok(())
}

fn handle_slave_cmd(action: SlaveCommand, config_path: &PathBuf) -> Result<()> {
    match action {
        SlaveCommand::Add {
            name,
            mut masters,
            channels,
            catch_all,
            default,
        } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let norm = normalize_sink_name(&name);
            if topology.find_slave(&norm).is_some() {
                bail!("Slave sink '{}' already exists.", norm);
            }
            if masters.is_empty() {
                if let Some(first_master) = topology.masters.first() {
                    masters.push(first_master.name.clone());
                }
            }
            if default {
                for s in &mut topology.slaves {
                    s.is_default_sink = false;
                }
            }
            if catch_all {
                for s in &mut topology.slaves {
                    s.is_catch_all = false;
                }
            }
            topology.slaves.push(SlaveSink {
                name: norm.clone(),
                description: None,
                channel_map: channels,
                masters: masters.clone(),
                is_default_sink: default,
                is_catch_all: catch_all,
            });
            topology.save(config_path)?;
            println!(
                "Added Slave sink '{}' ({}) -> Masters: [{}].",
                norm,
                channels,
                masters.join(", ")
            );
        }

        SlaveCommand::Rm { name } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let norm = normalize_sink_name(&name);
            let before = topology.slaves.len();
            topology
                .slaves
                .retain(|s| !normalize_sink_name(&s.name).eq_ignore_ascii_case(&norm));
            if topology.slaves.len() == before {
                bail!("Slave sink '{}' not found.", norm);
            }
            topology
                .rules
                .retain(|r| !normalize_sink_name(&r.target_slave).eq_ignore_ascii_case(&norm));
            topology.save(config_path)?;
            println!("Removed Slave sink '{}' and its routing rules.", norm);
        }

        SlaveCommand::List => {
            let topology = TopologyConfig::load_or_default(config_path)?;
            if topology.slaves.is_empty() {
                println!("No Slave sinks configured.");
            } else {
                for s in &topology.slaves {
                    let mut flags = Vec::new();
                    if s.is_catch_all {
                        flags.push("catch-all");
                    }
                    if s.is_default_sink {
                        flags.push("default");
                    }
                    let flag_str = if flags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", flags.join(", "))
                    };
                    println!(
                        "- {}{} (channels: {}, masters: [{}])",
                        s.name,
                        flag_str,
                        s.channel_map,
                        s.masters.join(", ")
                    );
                }
            }
        }

        SlaveCommand::Link { slave, master } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            if topology.find_master(&master).is_none() {
                bail!("Master sink '{}' does not exist.", master);
            }
            let s = topology
                .find_slave_mut(&slave)
                .with_context(|| format!("Slave sink '{}' not found", slave))?;
            if !s
                .masters
                .iter()
                .any(|m| m.eq_ignore_ascii_case(&master))
            {
                s.masters.push(master.clone());
            }
            topology.save(config_path)?;
            println!("Linked Slave '{}' ──► Master '{}'.", slave, master);
        }

        SlaveCommand::Unlink { slave, master } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let s = topology
                .find_slave_mut(&slave)
                .with_context(|| format!("Slave sink '{}' not found", slave))?;
            s.masters.retain(|m| !m.eq_ignore_ascii_case(&master));
            topology.save(config_path)?;
            println!("Unlinked Slave '{}' from Master '{}'.", slave, master);
        }

        SlaveCommand::SetDefault { slave } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            if topology.find_slave(&slave).is_none() {
                bail!("Slave sink '{}' not found.", slave);
            }
            let norm = normalize_sink_name(&slave);
            for s in &mut topology.slaves {
                s.is_default_sink = normalize_sink_name(&s.name).eq_ignore_ascii_case(&norm);
            }
            topology.save(config_path)?;
            println!("Set Slave '{}' as default Audio/Sink.", norm);
        }

        SlaveCommand::SetCatchAll { slave, disable } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let s = topology
                .find_slave_mut(&slave)
                .with_context(|| format!("Slave sink '{}' not found", slave))?;
            s.is_catch_all = !disable;
            let norm = s.name.clone();
            topology.save(config_path)?;
            println!(
                "Set catch_all = {} on Slave '{}'.",
                !disable, norm
            );
        }
    }
    Ok(())
}

fn handle_rule_cmd(action: RuleCommand, config_path: &PathBuf) -> Result<()> {
    match action {
        RuleCommand::Add {
            slave,
            app,
            bin,
            node,
            role,
            not_app,
            not_bin,
            not_node,
            description,
        } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let slave_obj = topology
                .find_slave(&slave)
                .with_context(|| format!("Slave sink '{}' not found", slave))?
                .clone();

            let mut clause = MatchClause::new();
            for v in app {
                clause.add_condition("application.name", v);
            }
            for v in bin {
                clause.add_condition("application.process.binary", v);
            }
            for v in node {
                clause.add_condition("node.name", v);
            }
            for v in role {
                clause.add_condition("media.role", v);
            }
            for v in not_app {
                clause.add_condition("application.name", format!("!{}", v.trim_start_matches('!')));
            }
            for v in not_bin {
                clause.add_condition(
                    "application.process.binary",
                    format!("!{}", v.trim_start_matches('!')),
                );
            }
            for v in not_node {
                clause.add_condition("node.name", format!("!{}", v.trim_start_matches('!')));
            }

            if clause.is_empty() {
                bail!("At least one match condition (--app, --bin, --node, --role, etc.) must be specified.");
            }

            let compact = clause.display_compact();
            if let Some(existing) = topology.rules.iter_mut().find(|r| {
                normalize_sink_name(&r.target_slave).eq_ignore_ascii_case(&slave_obj.name)
            }) {
                if description.is_some() {
                    existing.set_description = description;
                }
                if !existing.matches.contains(&clause) {
                    existing.matches.push(clause);
                }
            } else {
                topology.rules.push(RoutingRule {
                    id: slave_obj.name.to_ascii_lowercase(),
                    target_slave: slave_obj.name.clone(),
                    set_description: description,
                    matches: vec![clause],
                });
            }

            topology.save(config_path)?;
            println!(
                "Added routing rule for Slave '{}': {}",
                slave_obj.name, compact
            );
        }

        RuleCommand::List { slave } => {
            let topology = TopologyConfig::load_or_default(config_path)?;
            for s in &topology.slaves {
                if let Some(ref filter) = slave {
                    if !normalize_sink_name(&s.name)
                        .eq_ignore_ascii_case(&normalize_sink_name(filter))
                    {
                        continue;
                    }
                }
                let rules = topology.rules_for_slave(&s.name);
                let clauses: Vec<_> = rules.iter().flat_map(|r| r.matches.iter()).collect();
                println!("Slave '{}' (catch_all: {}):", s.name, s.is_catch_all);
                if s.is_catch_all {
                    let auto_c = topology.compute_catch_all_clause(&s.name);
                    let total_neg: usize = auto_c.conditions.values().map(|v| v.len()).sum();
                    println!(
                        "  [auto] <catch-all inverse clause with {} negated conditions>",
                        total_neg
                    );
                }
                if clauses.is_empty() {
                    println!("  (no explicit match clauses)");
                } else {
                    for (idx, c) in clauses.iter().enumerate() {
                        println!("  [{}] {}", idx + 1, c.display_compact());
                    }
                }
            }
        }

        RuleCommand::Rm { slave, index, all } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let norm = normalize_sink_name(&slave);
            if all {
                topology
                    .rules
                    .retain(|r| !normalize_sink_name(&r.target_slave).eq_ignore_ascii_case(&norm));
                topology.save(config_path)?;
                println!("Removed all explicit routing rules for Slave '{}'.", norm);
                return Ok(());
            }

            let Some(idx_1based) = index else {
                bail!("Specify a 1-based clause index to remove, or pass `--all`.");
            };
            let Some(rule) = topology
                .rules
                .iter_mut()
                .find(|r| normalize_sink_name(&r.target_slave).eq_ignore_ascii_case(&norm))
            else {
                bail!("No rules found for Slave '{}'.", norm);
            };
            if idx_1based == 0 || idx_1based > rule.matches.len() {
                bail!(
                    "Invalid clause index {}. Slave '{}' has {} clause(s).",
                    idx_1based,
                    norm,
                    rule.matches.len()
                );
            }
            let removed = rule.matches.remove(idx_1based - 1);
            topology.save(config_path)?;
            println!(
                "Removed clause [{}] ({}) from Slave '{}'.",
                idx_1based,
                removed.display_compact(),
                norm
            );
        }

        RuleCommand::AssignRunning { query, slave } => {
            let mut topology = TopologyConfig::load_or_default(config_path)?;
            let slave_obj = topology
                .find_slave(&slave)
                .with_context(|| format!("Slave sink '{}' not found", slave))?
                .clone();

            let graph = LiveGraph::snapshot()?;
            let streams = graph.playback_streams();
            let q_lower = query.to_ascii_lowercase();
            let matched_stream = streams.into_iter().find(|st| {
                st.props
                    .get("application.process.id")
                    .map(|p| p == &query)
                    .unwrap_or(false)
                    || st
                        .props
                        .get("application.process.binary")
                        .map(|b| b.to_ascii_lowercase().contains(&q_lower))
                        .unwrap_or(false)
                    || st
                        .props
                        .get("application.name")
                        .map(|a| a.to_ascii_lowercase().contains(&q_lower))
                        .unwrap_or(false)
                    || st.name.to_ascii_lowercase().contains(&q_lower)
            });

            let Some(stream) = matched_stream else {
                bail!(
                    "No active playback stream matching '{}' found in PipeWire.",
                    query
                );
            };

            let mut clause = MatchClause::new();
            if let Some(bin) = stream.props.get("application.process.binary") {
                clause.add_condition("application.process.binary", bin.clone());
            } else if let Some(app) = stream.props.get("application.name") {
                clause.add_condition("application.name", app.clone());
            } else {
                clause.add_condition("node.name", stream.name.clone());
            }

            let compact = clause.display_compact();
            if let Some(existing) = topology.rules.iter_mut().find(|r| {
                normalize_sink_name(&r.target_slave).eq_ignore_ascii_case(&slave_obj.name)
            }) {
                if !existing.matches.contains(&clause) {
                    existing.matches.push(clause);
                }
            } else {
                topology.rules.push(RoutingRule {
                    id: slave_obj.name.to_ascii_lowercase(),
                    target_slave: slave_obj.name.clone(),
                    set_description: None,
                    matches: vec![clause],
                });
            }

            topology.save(config_path)?;
            println!(
                "Assigned stream '{}' ({}) to Slave '{}' with rule: {}",
                stream.description, stream.name, slave_obj.name, compact
            );
        }
    }
    Ok(())
}

fn print_sync_report(report: &nullroute_core::runtime::SyncReport, dry_run: bool) {
    let prefix = if dry_run { "[DRY RUN] " } else { "" };
    if report.created_sinks.is_empty()
        && report.created_links.is_empty()
        && report.removed_links.is_empty()
        && report.rerouted_streams.is_empty()
        && report.default_sink_updated.is_none()
    {
        println!(
            "{}Live PipeWire graph is already in sync with topology.",
            prefix
        );
        return;
    }

    for s in &report.created_sinks {
        println!("{}Created live virtual sink: {}", prefix, s);
    }
    for l in &report.created_links {
        println!("{}Linked: {}", prefix, l);
    }
    for l in &report.removed_links {
        println!("{}Unlinked stale connection: {}", prefix, l);
    }
    if let Some(def) = &report.default_sink_updated {
        println!("{}Set default Audio/Sink: {}", prefix, def);
    }
    for r in &report.rerouted_streams {
        println!("{}Rerouted active stream: {}", prefix, r);
    }
}
