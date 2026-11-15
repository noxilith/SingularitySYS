//! What forge prints: colour, symbols, spinners and tables when the terminal can show them, plain text when the
//! output is piped (so scripts and tests read stable lines). The Linux console gets colour but ASCII symbols,
//! because its font lacks most of Unicode.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use forge_apply::{Change, Kind, Plan, Progress};

pub struct Ui {
    pub tty: bool,
    pub color: bool,
    pub unicode: bool,
}

pub fn ui() -> &'static Ui {
    static UI: OnceLock<Ui> = OnceLock::new();
    UI.get_or_init(detect)
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn detect() -> Ui {
    let tty = std::io::stdout().is_terminal();
    let term = env("TERM");
    let color = tty && std::env::var_os("NO_COLOR").is_none() && !term.is_empty() && term != "dumb";
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"].iter().map(|n| env(n)).find(|v| !v.is_empty()).unwrap_or_default().to_lowercase();
    let utf8 = locale.contains("utf-8") || locale.contains("utf8");
    Ui { tty, color, unicode: tty && utf8 && term != "linux" }
}

impl Ui {
    fn paint(&self, code: &str, s: &str) -> String {
        if self.color { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn cyan(&self, s: &str) -> String {
        self.paint("36", s)
    }
    fn title(&self, s: &str) -> String {
        self.paint("1;36", s)
    }

    pub fn arrow(&self) -> &'static str {
        if self.unicode { "→" } else { "->" }
    }
    fn rule(&self, n: usize) -> String {
        self.dim(&(if self.unicode { "─" } else { "-" }).repeat(n))
    }
    fn ok_mark(&self) -> String {
        self.green(if self.unicode { "✓" } else { "ok" })
    }
    fn fail_mark(&self) -> String {
        self.red(if self.unicode { "✗" } else { "!!" })
    }
    fn warn_mark(&self) -> String {
        self.yellow("!")
    }
    fn info_mark(&self) -> String {
        self.cyan(if self.unicode { "•" } else { "*" })
    }
    pub fn on(&self) -> String {
        self.green(if self.unicode { "●" } else { "*" })
    }
    pub fn off(&self) -> String {
        self.yellow(if self.unicode { "○" } else { "o" })
    }
    pub fn dot(&self) -> String {
        self.dim(if self.unicode { "·" } else { "-" })
    }
    fn kind_mark(&self, k: Kind) -> String {
        match k {
            Kind::Add => self.green("+"),
            Kind::Modify => self.yellow("~"),
            Kind::Remove => self.red("-"),
        }
    }
}

/// A section title with a rule under it.
pub fn heading(text: &str) {
    let u = ui();
    println!();
    println!("{}", u.title(text));
    println!("{}", u.rule(text.chars().count().max(8)));
}

pub fn ok(text: &str) {
    println!("{} {text}", ui().ok_mark());
}

pub fn warn(text: &str) {
    eprintln!("{} {text}", ui().warn_mark());
}

pub fn info(text: &str) {
    println!("{} {text}", ui().info_mark());
}

/// What to do next, set apart.
pub fn next(text: &str) {
    let u = ui();
    println!("{} {}", u.cyan(u.arrow()), u.bold(text));
}

pub fn dim(text: &str) {
    println!("{}", ui().dim(text));
}

/// `[y/N]` question; anything but yes is no.
pub fn confirm(question: &str) -> std::io::Result<bool> {
    use std::io::BufRead;
    print!("{} {} ", ui().bold(question), ui().dim("[y/N]"));
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

pub fn duration(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 10.0 {
        format!("{s:.1}s")
    } else if s < 120.0 {
        format!("{:.0}s", s)
    } else {
        format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

// ---- spinner ----------------------------------------------------------------

/// A line that shows work in progress (a spinner and the elapsed time on a terminal) and is replaced by a
/// result line when done. Piped output gets just the result line.
pub struct Spinner {
    msg: String,
    start: Instant,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Spinner {
    pub fn start(msg: impl Into<String>) -> Spinner {
        let msg = msg.into();
        let stop = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        let handle = ui().tty.then(|| {
            let (stop, msg) = (stop.clone(), msg.clone());
            std::thread::spawn(move || {
                let u = ui();
                let frames: &[&str] = if u.unicode { &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"] } else { &["|", "/", "-", "\\"] };
                let mut i = 0;
                while !stop.load(Ordering::Relaxed) {
                    let elapsed = start.elapsed();
                    let t = if elapsed.as_secs() >= 2 { format!(" {}", u.dim(&duration(elapsed))) } else { String::new() };
                    print!("\r\x1b[2K{} {msg}{t}", u.cyan(frames[i % frames.len()]));
                    let _ = std::io::stdout().flush();
                    i += 1;
                    std::thread::sleep(Duration::from_millis(90));
                }
            })
        });
        Spinner { msg, start, stop, handle }
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
            print!("\r\x1b[2K");
            let _ = std::io::stdout().flush();
        }
    }

    pub fn finish(mut self) {
        self.halt();
        let u = ui();
        println!("{} {} {}", u.ok_mark(), self.msg, u.dim(&duration(self.start.elapsed())));
    }

    pub fn fail(mut self) {
        self.halt();
        println!("{} {}", ui().fail_mark(), self.msg);
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.halt();
    }
}

// ---- plans ------------------------------------------------------------------

fn split_id(id: &forge_store::StoreId) -> (String, String) {
    (id.name().to_string(), id.version().to_string())
}

/// The packages a plan installs, upgrades and removes: (installed, upgraded old->new, removed).
#[derive(Default)]
pub struct PackageChanges {
    pub installed: Vec<(String, String)>,
    pub upgraded: Vec<(String, String, String)>,
    pub removed: Vec<(String, String)>,
}

pub fn package_changes(plan: &Plan) -> PackageChanges {
    let mut added: BTreeMap<String, String> = BTreeMap::new();
    let mut removed: BTreeMap<String, String> = BTreeMap::new();
    for c in &plan.changes {
        let Some(id) = c.package_id() else { continue };
        let (name, version) = split_id(&id);
        match c.kind {
            Kind::Remove => {
                removed.insert(name, version);
            }
            _ => {
                added.insert(name, version);
            }
        }
    }
    let mut out = PackageChanges::default();
    for (name, new) in added {
        match removed.remove(&name) {
            Some(old) => out.upgraded.push((name, old, new)),
            None => out.installed.push((name, new)),
        }
    }
    out.removed = removed.into_iter().collect();
    out
}

/// Show what applying `plan` will do: the packages asked for (and removed), their dependencies, then the other
/// changes. `requested` are the names in the declaration; anything else that gets installed is a dependency.
pub fn print_plan(plan: &Plan, requested: &[String]) {
    let u = ui();
    let pk = package_changes(plan);
    let is_req = |n: &str| requested.iter().any(|r| r == n);
    let width = pk
        .installed
        .iter()
        .map(|(n, _)| n.len())
        .chain(pk.upgraded.iter().map(|(n, _, _)| n.len()))
        .chain(pk.removed.iter().map(|(n, _)| n.len()))
        .max()
        .unwrap_or(0);
    let line = |mark: Kind, name: &str, rest: String, dim: bool| {
        let name = if dim { u.dim(&format!("{name:<width$}")) } else { format!("{name:<width$}") };
        println!("  {} {name}  {rest}", u.kind_mark(mark));
    };
    let show_upgrade = |n: &str, old: &str, new: &str, dim: bool| {
        let rest = if old == new { format!("{new} {}", u.dim("(rebuilt)")) } else { format!("{} {} {new}", u.dim(old), u.arrow()) };
        line(Kind::Modify, n, rest, dim);
    };

    let main_count = pk.installed.iter().filter(|(n, _)| is_req(n)).count() + pk.upgraded.iter().filter(|(n, _, _)| is_req(n)).count() + pk.removed.len();
    if main_count > 0 {
        heading("Packages");
        for (n, v) in pk.installed.iter().filter(|(n, _)| is_req(n)) {
            line(Kind::Add, n, v.clone(), false);
        }
        for (n, old, new) in pk.upgraded.iter().filter(|(n, _, _)| is_req(n)) {
            show_upgrade(n, old, new, false);
        }
        for (n, v) in &pk.removed {
            line(Kind::Remove, n, u.dim(v), false);
        }
    }
    let deps_new: Vec<_> = pk.installed.iter().filter(|(n, _)| !is_req(n)).collect();
    let deps_up: Vec<_> = pk.upgraded.iter().filter(|(n, _, _)| !is_req(n)).collect();
    if !deps_new.is_empty() || !deps_up.is_empty() {
        heading(&format!("Dependencies ({})", deps_new.len() + deps_up.len()));
        for (n, v) in deps_new {
            line(Kind::Add, n, u.dim(v), true);
        }
        for (n, old, new) in deps_up {
            show_upgrade(n, old, new, true);
        }
    }

    let others: Vec<&Change> = plan.changes.iter().filter(|c| c.section != "packages").collect();
    if !others.is_empty() {
        heading("System");
        let w = others.iter().map(|c| c.section.len()).max().unwrap_or(0);
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        let mut hidden: BTreeMap<&str, usize> = BTreeMap::new();
        for c in &others {
            let n = seen.entry(c.section).or_insert(0);
            *n += 1;
            if *n > 4 {
                *hidden.entry(c.section).or_insert(0) += 1;
                continue;
            }
            println!("  {} {}  {}", u.kind_mark(c.kind), u.dim(&format!("{:<w$}", c.section)), c.summary);
        }
        for (section, n) in hidden {
            println!("  {} {}  {}", u.dim(" "), u.dim(&format!("{section:<w$}")), u.dim(&format!("... and {n} more")));
        }
    }

    let mut parts = Vec::new();
    let installs = pk.installed.len();
    let deps = pk.installed.iter().filter(|(n, _)| !is_req(n)).count();
    if installs > 0 {
        parts.push(if deps > 0 { format!("{} to install (+{deps} dependencies)", installs - deps) } else { format!("{installs} to install") });
    }
    for (n, what) in [(pk.upgraded.len(), "to upgrade"), (pk.removed.len(), "to remove")] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    if !others.is_empty() {
        parts.push(format!("{} system change{}", others.len(), if others.len() == 1 { "" } else { "s" }));
    }
    if !parts.is_empty() {
        println!();
        println!("{}", u.bold(&parts.join(", ")));
    }
}

/// Carry out a plan with one line per package and a spinner while it works.
pub fn apply_plan(plan: &Plan) -> Result<(), forge_apply::ApplyError> {
    let pkg_total = plan.changes.iter().filter(|c| c.package_id().is_some() && c.summary.contains("(install from")).count();
    let mut pkg_index = 0;
    let mut current: Option<Spinner> = None;
    println!();
    let result = forge_apply::apply_with(plan, &mut |p| match p {
        Progress::Start { change, .. } => {
            if let Some(id) = change.package_id() {
                pkg_index += 1;
                let verb = if change.downloads() { "downloading" } else { "installing" };
                let (name, version) = split_id(&id);
                current = Some(Spinner::start(format!("[{pkg_index}/{pkg_total}] {verb} {name} {version}")));
            }
        }
        Progress::Done { .. } => {
            if let Some(s) = current.take() {
                s.finish();
            }
        }
        Progress::Copying => current = Some(Spinner::start("copying the packages into the system")),
        Progress::Recording => {
            if let Some(s) = current.take() {
                s.finish();
            }
            current = Some(Spinner::start("recording the new package set"));
        }
    });
    match result {
        Ok(()) => {
            if let Some(s) = current.take() {
                s.finish();
            }
            Ok(())
        }
        Err(e) => {
            if let Some(s) = current.take() {
                s.fail();
            }
            Err(e)
        }
    }
}
