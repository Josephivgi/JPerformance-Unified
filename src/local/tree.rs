//! Processes as a tree: which app started what. Lets the user stop one agent session
//! with all its helpers, or a helper left running after its app closed, without
//! touching other processes that happen to share its name.

use super::procs::{self, Proc, StopResult};
use std::collections::{HashMap, HashSet};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Runtimes and shells that are only ever started by another program. One of these
/// still running after the program that started it has exited is left over.
const HELPERS: &[&str] = &[
    "node", "python", "pythonw", "py", "cmd", "conhost", "pwsh", "powershell", "bash", "sh", "uv", "uvx", "npx",
    "npm", "pnpm", "yarn", "deno", "bun", "esbuild", "git", "ssh", "openconsole", "ruby", "perl", "tsx",
    // Parts of shell pipelines that agents and scripts run.
    "head", "tail", "grep", "sleep", "sed", "awk", "cat", "find", "xargs", "tee", "sort", "wc", "less", "watch",
    "timeout", "ping", "curl", "rg", "fd", "jq",
];

/// Coding agents that run in a terminal. Each session keeps its own set of helper processes.
const AGENTS: &[&str] = &[
    "claude", "codex", "droid", "gemini", "opencode", "cursor-agent", "qwen", "grok", "kilo", "goose", "cline",
    "kimi", "devin", "aider", "amp", "copilot", "crush",
];

/// A left-over helper is suggested for clean-up once it has been idle this long.
pub const ORPHAN_IDLE_SECS: i64 = 10 * 60;
/// An agent session is suggested once it has not used the CPU for this long.
pub const AGENT_IDLE_SECS: i64 = 30 * 60;

/// Words in a command line that mean a helper is a server or watcher someone may still use
/// (a dev server started from a terminal that was closed), not a leftover.
const LONG_RUNNING: &[&str] = &["serve", "server", "dev", "start", "daemon", "watch", "hub", "listen", "tunnel", "worker"];

/// True when a command line does not look like a server or watcher.
fn looks_long_running(cmdline: &str) -> bool {
    LONG_RUNNING.iter().any(|w| cmdline.contains(w))
}

fn base(name: &str) -> String {
    name.to_ascii_lowercase().trim_end_matches(".exe").to_string()
}
/// Shells left behind by a closed program. Only these are checked by default: a node or python
/// process may be a bot or a server someone still uses, so the user ticks those by hand.
pub fn is_shell(name: &str) -> bool {
    matches!(base(name).as_str(), "cmd" | "pwsh" | "powershell" | "bash" | "sh")
}
pub fn is_helper(name: &str) -> bool {
    HELPERS.contains(&base(name).as_str())
}
pub fn is_agent(name: &str) -> bool {
    AGENTS.contains(&base(name).as_str())
}

pub struct Node {
    pub pid: u32,
    pub started: i64,
    /// Set only when the parent is still running (and is not a newer process that reused its id).
    pub parent: Option<u32>,
    /// The program that started it has exited.
    pub orphan: bool,
    pub name: String,
    pub exe: Option<String>,
    pub cpu: f64,
    pub mem: u64,
    pub detail: String,
    /// The whole command line, lowercased. Only used to spot servers and watchers; never shown.
    pub cmdline: String,
    pub session: Option<u32>,
    /// Seconds since it last used the CPU, as recorded by the background recorder (0 when unknown).
    pub idle: i64,
}

pub struct Tree {
    pub nodes: HashMap<u32, Node>,
    pub children: HashMap<u32, Vec<u32>>,
    windows: HashMap<u32, Vec<String>>,
}

/// What a program was started with, short enough for one line: the folder an agent works in,
/// or the script a runtime runs.
fn detail(p: &sysinfo::Process) -> String {
    let name = base(&p.name().to_string_lossy());
    let cwd = p.cwd().and_then(|c| c.file_name()).map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    if AGENTS.contains(&name.as_str()) {
        return if cwd.is_empty() { String::new() } else { format!("in {cwd}") };
    }
    let arg = p
        .cmd()
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .find(|a| !a.starts_with('-') && !a.is_empty());
    let Some(arg) = arg else { return String::new() };
    let short = if arg.contains(['\\', '/']) {
        let parts: Vec<&str> = arg.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
        parts[parts.len().saturating_sub(2)..].join("/")
    } else {
        arg
    };
    short.chars().take(70).collect()
}

impl Tree {
    /// Builds the tree from an already refreshed process list. `activity` maps
    /// (pid, start time) to when that process last used the CPU.
    pub fn build(sys: &System, cores: f64, activity: &HashMap<(u32, i64), i64>, now: i64) -> Tree {
        let mut nodes = HashMap::new();
        for (pid, p) in sys.processes() {
            let pid = pid.as_u32();
            if pid == 0 {
                continue;
            }
            let started = p.start_time() as i64;
            let (parent, orphan) = match p.parent() {
                None => (None, false),
                Some(pp) => match sys.process(pp) {
                    Some(par) if (par.start_time() as i64) <= started && pp.as_u32() != pid => (Some(pp.as_u32()), false),
                    // Ids 0 and 4 are the kernel; their "children" are early boot processes.
                    _ => (None, pp.as_u32() > 4),
                },
            };
            nodes.insert(
                pid,
                Node {
                    pid,
                    started,
                    parent,
                    orphan,
                    name: p.name().to_string_lossy().to_string(),
                    exe: p.exe().map(|e| e.display().to_string()),
                    cpu: p.cpu_usage() as f64 / cores,
                    mem: p.memory(),
                    detail: detail(p),
                    cmdline: p.cmd().iter().map(|a| a.to_string_lossy().to_ascii_lowercase()).collect::<Vec<_>>().join(" "),
                    session: p.session_id().map(|s| s.as_u32()),
                    idle: activity.get(&(pid, started)).map(|&at| (now - at).max(0)).unwrap_or(0),
                },
            );
        }
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for n in nodes.values() {
            if let Some(par) = n.parent {
                children.entry(par).or_default().push(n.pid);
            }
        }
        Tree { nodes, children, windows: procs::window_titles() }
    }

    /// `pid` and everything it started, parents before children.
    pub fn descendants(&self, pid: u32) -> Vec<u32> {
        let mut out = vec![pid];
        let mut i = 0;
        let mut seen = HashSet::from([pid]);
        while i < out.len() {
            for &c in self.children.get(&out[i]).into_iter().flatten() {
                if seen.insert(c) {
                    out.push(c);
                }
            }
            i += 1;
        }
        out
    }

    /// The parents of `pid`, nearest first.
    pub fn ancestors(&self, pid: u32) -> impl Iterator<Item = u32> + '_ {
        let mut cur = self.nodes.get(&pid).and_then(|n| n.parent);
        let mut steps = 0;
        std::iter::from_fn(move || {
            let c = cur?;
            steps += 1;
            if steps > 64 {
                return None;
            }
            cur = self.nodes.get(&c).and_then(|n| n.parent);
            Some(c)
        })
    }

    fn killable(&self, pid: u32) -> bool {
        self.nodes.get(&pid).is_some_and(|n| procs::tree_killable(&n.name, n.exe.as_deref()))
    }

    /// The app a process belongs to: its highest ancestor that is not part of Windows.
    pub fn owner(&self, pid: u32) -> u32 {
        let mut cur = pid;
        let mut steps = 0;
        while let Some(par) = self.nodes.get(&cur).and_then(|n| n.parent) {
            if !self.killable(par) || steps > 64 {
                break;
            }
            cur = par;
            steps += 1;
        }
        cur
    }

    pub fn title(&self, pid: u32) -> Option<&str> {
        self.windows.get(&pid).and_then(|t| t.first()).map(String::as_str)
    }

    /// Totals for `pid` and everything under it.
    pub fn sum(&self, pid: u32) -> Sum {
        let mut s = Sum { idle: i64::MAX, ..Default::default() };
        for p in self.descendants(pid) {
            if let Some(n) = self.nodes.get(&p) {
                s.cpu += n.cpu;
                s.mem += n.mem;
                s.procs += 1;
                s.idle = s.idle.min(n.idle);
                s.window |= self.windows.contains_key(&p);
            }
        }
        if s.idle == i64::MAX {
            s.idle = 0;
        }
        s
    }

    /// True when the whole tree is small and nothing in it looks like a server or watcher.
    /// Children are checked too: a shell that only wraps a dev server must not be pre-checked.
    pub fn tree_safe(&self, pid: u32) -> bool {
        let all = self.descendants(pid);
        all.len() <= 3 && all.iter().all(|p| self.nodes.get(p).is_none_or(|n| !looks_long_running(&n.cmdline)))
    }

    fn label(&self, pid: u32) -> String {
        let n = &self.nodes[&pid];
        if n.detail.is_empty() { n.name.clone() } else { format!("{} {}", n.name, n.detail) }
    }

    /// Every app with the processes it started, the heaviest first. An app's sessions
    /// (child processes that started their own helpers) are listed so they can be stopped one at a time.
    pub fn groups(&self, my_session: Option<u32>) -> Vec<Group> {
        let mut roots: HashMap<u32, ()> = HashMap::new();
        for &pid in self.nodes.keys() {
            if self.killable(pid) {
                roots.insert(self.owner(pid), ());
            }
        }
        let mut out: Vec<Group> = roots
            .into_keys()
            .filter_map(|root| {
                let n = self.nodes.get(&root)?;
                if !self.killable(root) || (my_session.is_some() && n.session != my_session) {
                    return None;
                }
                let s = self.sum(root);
                // Agent sessions anywhere under the app are shown on their own, since each can be stopped alone.
                let sessions: Vec<u32> = self
                    .descendants(root)
                    .into_iter()
                    .skip(1)
                    .filter(|&p| {
                        self.nodes.get(&p).is_some_and(|n| is_agent(&n.name))
                            && !self.ancestors(p).any(|a| a != root && self.nodes.get(&a).is_some_and(|n| is_agent(&n.name)))
                    })
                    .collect();
                let inside: HashSet<u32> = sessions.iter().flat_map(|&p| self.ancestors(p)).collect();
                let mut branches: Vec<Branch> = sessions
                    .iter()
                    .chain(self.children.get(&root).into_iter().flatten().filter(|c| !inside.contains(c) && !sessions.contains(c)))
                    .filter_map(|&c| {
                        let cn = self.nodes.get(&c)?;
                        let cs = self.sum(c);
                        // Plain worker processes of the same program (browser tabs) are not sessions.
                        (cs.procs > 1 || base(&cn.name) != base(&n.name) || is_agent(&cn.name)).then(|| Branch {
                            pid: c,
                            started: cn.started,
                            name: cn.name.clone(),
                            label: self.label(c),
                            title: self.title(c).map(str::to_string),
                            sum: cs,
                            agent: is_agent(&cn.name),
                        })
                    })
                    .collect();
                branches.sort_by(|a, b| b.sum.mem.cmp(&a.sum.mem));
                Some(Group {
                    pid: root,
                    started: n.started,
                    name: n.name.clone(),
                    label: self.label(root),
                    title: self.title(root).map(str::to_string),
                    orphan: n.orphan,
                    sum: s,
                    branches,
                })
            })
            .collect();
        out.sort_by(|a, b| b.sum.mem.cmp(&a.sum.mem));
        out
    }

    /// Processes worth stopping: helpers whose app has exited, and agent sessions nobody has used for a while.
    pub fn suggestions(&self, my_session: Option<u32>) -> Vec<Suggestion> {
        let mut out = Vec::new();
        for n in self.nodes.values() {
            if (my_session.is_some() && n.session != my_session) || !self.killable(n.pid) {
                continue;
            }
            if n.orphan {
                let s = self.sum(n.pid);
                // Something with a window is in use even if its launcher exited, and other programs
                // (OneDrive, vendor tools) are often started by a launcher that exits on purpose.
                if s.window || !is_helper(&n.name) {
                    continue;
                }
                out.push(Suggestion {
                    pid: n.pid,
                    started: n.started,
                    name: n.name.clone(),
                    label: self.label(n.pid),
                    kind: "orphan",
                    why: "Left running after the program that started it closed".into(),
                    checked: s.idle >= ORPHAN_IDLE_SECS && is_shell(&n.name) && self.tree_safe(n.pid),
                    sum: s,
                });
            } else if is_agent(&n.name) && !n.parent.is_some_and(|p| self.nodes.get(&p).is_some_and(|pn| is_agent(&pn.name))) {
                let s = self.sum(n.pid);
                if s.idle >= AGENT_IDLE_SECS {
                    out.push(Suggestion {
                        pid: n.pid,
                        started: n.started,
                        name: n.name.clone(),
                        label: self.label(n.pid),
                        kind: "idle_agent",
                        why: "Agent session that has not done anything for a while".into(),
                        checked: false,
                        sum: s,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.sum.mem.cmp(&a.sum.mem));
        out
    }

    /// Handles to `pid` and everything it started that may be ended, parents first.
    fn open_tree(&self, pid: u32) -> Vec<Proc> {
        self.descendants(pid)
            .into_iter()
            .filter(|&p| self.killable(p))
            .filter_map(|p| Proc::open(p, self.nodes.get(&p).and_then(|n| n.parent)))
            .collect()
    }
}

#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct Sum {
    pub cpu: f64,
    pub mem: u64,
    pub procs: usize,
    pub idle: i64,
    pub window: bool,
}

#[derive(serde::Serialize)]
pub struct Branch {
    pub pid: u32,
    pub started: i64,
    pub name: String,
    pub label: String,
    pub title: Option<String>,
    pub sum: Sum,
    pub agent: bool,
}

#[derive(serde::Serialize)]
pub struct Group {
    pub pid: u32,
    pub started: i64,
    pub name: String,
    pub label: String,
    pub title: Option<String>,
    pub orphan: bool,
    pub sum: Sum,
    pub branches: Vec<Branch>,
}

#[derive(serde::Serialize)]
pub struct Suggestion {
    pub pid: u32,
    pub started: i64,
    pub name: String,
    pub label: String,
    pub kind: &'static str,
    pub why: String,
    pub checked: bool,
    pub sum: Sum,
}

pub struct TreeStop {
    pub pid: u32,
    pub name: String,
    pub procs: usize,
    pub mem: u64,
    pub result: StopResult,
}

/// The current user's Windows session, so other users' and services' processes are left alone.
pub fn my_session() -> Option<u32> {
    let mut sys = System::new();
    let me = Pid::from_u32(std::process::id());
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[me]), false, ProcessRefreshKind::nothing());
    sys.process(me).and_then(|p| p.session_id()).map(|s| s.as_u32())
}

pub fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet)
        .with_cwd(UpdateKind::OnlyIfNotSet)
}

/// Stops each (pid, start time) with everything it started, all at once.
/// A pid that now belongs to a different process is skipped.
pub fn stop_trees(tree: &Tree, targets: &[(u32, i64)], force: bool) -> Vec<TreeStop> {
    let jobs: Vec<(u32, String, usize, u64, Vec<Proc>)> = targets
        .iter()
        .filter_map(|&(pid, started)| {
            let n = tree.nodes.get(&pid).filter(|n| n.started == started)?;
            let s = tree.sum(pid);
            Some((pid, n.name.clone(), s.procs, s.mem, if tree.killable(pid) { tree.open_tree(pid) } else { Vec::new() }))
        })
        .collect();
    let mut out: Vec<TreeStop> = targets
        .iter()
        .filter(|(pid, started)| !tree.nodes.get(pid).is_some_and(|n| n.started == *started))
        .map(|&(pid, _)| TreeStop { pid, name: String::new(), procs: 0, mem: 0, result: StopResult::NotRunning })
        .collect();
    std::thread::scope(|sc| {
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(pid, name, n, mem, procs)| {
                let protected = !tree.killable(pid);
                sc.spawn(move || {
                    let result = if protected { StopResult::Protected } else { procs::stop_procs(procs, force) };
                    TreeStop { pid, name, procs: n, mem, result }
                })
            })
            .collect();
        for h in handles {
            if let Ok(r) = h.join() {
                out.push(r);
            }
        }
    });
    out
}

/// A fresh process list with what the tree needs.
pub fn snapshot() -> System {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    sys
}
