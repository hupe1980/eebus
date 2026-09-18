//! The two §14a simulators, driven against each other.
//!
//! `examples/heat_pump.rs` and `examples/steuerbox.rs` are the only place the stack is
//! wired the way a device wires it: two processes, two identities on disk, a listener, an
//! announcement and a limit that has to be acknowledged. Everything else in the suite is
//! one process holding both ends.
//!
//! No multicast. The box is pointed at an address with `--dial`, because a CI runner is
//! exactly the sort of host where mDNS does not work, and a gate that skips is not a gate.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PUMP_PORT: u16 = 14712;
const BOX_PORT: u16 = 14713;

/// A child process and the file its output is going to.
struct Sim {
    child: Child,
    log: PathBuf,
}

impl Sim {
    fn output(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn run(root: &Path) -> Result<String, String> {
    build(root)?;
    let work = root.join("target/simulators");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;

    let mut report = String::new();

    // Pass one: each simulator mints and persists an identity, and prints its SKI.
    let (pump_ski, box_ski) = {
        let mut pump = spawn(
            root,
            &work,
            "heat_pump",
            "p0",
            &["--port", &PUMP_PORT.to_string()],
        )?;
        let mut boxed = spawn(
            root,
            &work,
            "steuerbox",
            "b0",
            &[
                "--port",
                &BOX_PORT.to_string(),
                "--dial",
                &format!("127.0.0.1:{PUMP_PORT}"),
            ],
        )?;
        let pump_ski = wait_for_ski(&pump, "the heat pump")?;
        let box_ski = wait_for_ski(&boxed, "the control box")?;

        // The control box binds the port it announces. SHIP §8.1 gives no node an opt-out:
        // §623 puts the floor at one active connection, §624 and §628 require a listening
        // TCP server, and §638 wants it open whenever the node is under its limit. A node
        // announcing `_ship._tcp` at a port it does not serve invites every peer on the
        // segment to retry for ever.
        let address: SocketAddr = format!("127.0.0.1:{BOX_PORT}").parse().unwrap();
        if !reachable(address) {
            return Err(format!(
                "the control box announces {BOX_PORT} and does not listen on it\n{}",
                boxed.output()
            ));
        }
        report.push_str("the control box serves the port it announces\n");

        pump.stop();
        boxed.stop();
        (pump_ski, box_ski)
    };

    // Pass two: each trusts the other, as after commissioning, and the exchange runs.
    let pump = spawn(
        root,
        &work,
        "heat_pump",
        "p1",
        &["--port", &PUMP_PORT.to_string(), "--trust", &box_ski],
    )?;
    let boxed = spawn(
        root,
        &work,
        "steuerbox",
        "b1",
        &[
            "--port",
            &BOX_PORT.to_string(),
            "--dial",
            &format!("127.0.0.1:{PUMP_PORT}"),
            "--trust",
            &pump_ski,
            "--limit",
            "4200",
        ],
    )?;

    // The §14a evidence: the appliance acknowledged the limit, and says so on the hooks a
    // certification laboratory reads.
    await_line(&boxed, "4200 W accepted", "the control box")?;
    await_line(&pump, "lpc:state limited", "the heat pump")?;
    await_line(&pump, "lpc:limit 4200 W", "the heat pump")?;

    // And nothing met itself. The box announces and browses, so it sees its own record.
    let log = boxed.output();
    if log.contains(&box_ski) {
        return Err(format!(
            "the control box reported its own SKI as a peer\n{log}"
        ));
    }
    for bad in ["Connection refused", "SelfConnection", "AddressConflict"] {
        if log.contains(bad) {
            return Err(format!("the control box logged `{bad}`\n{log}"));
        }
    }

    report.push_str("the §14a exchange completed between the two simulators:\n");
    for line in log
        .lines()
        .filter(|l| l.contains("accepted") || l.contains("bound to") || l.contains("plays the"))
    {
        report.push_str("  ");
        report.push_str(line.trim());
        report.push('\n');
    }
    Ok(report)
}

fn build(root: &Path) -> Result<(), String> {
    let status = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "build",
            "--example",
            "heat_pump",
            "--example",
            "steuerbox",
            "--features",
            "full",
        ])
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "the simulators did not build".to_string())
}

fn spawn(root: &Path, work: &Path, example: &str, tag: &str, args: &[&str]) -> Result<Sim, String> {
    let log = work.join(format!("{tag}.log"));
    let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
    let errors = file.try_clone().map_err(|e| e.to_string())?;
    let child = Command::new(root.join("target/debug/examples").join(example))
        .current_dir(root)
        .arg("--state")
        .arg(work.join(example))
        .args(args)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(errors)
        .spawn()
        .map_err(|e| format!("could not start {example}: {e}"))?;
    Ok(Sim { child, log })
}

/// Whether anything is accepting on `address`.
///
/// A connect rather than a shell out to `nc`: a check that is skipped when a tool is
/// missing is a check that reports success on the hosts least likely to have been tried.
fn reachable(address: SocketAddr) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn wait_for_ski(sim: &Sim, who: &str) -> Result<String, String> {
    let line = await_line(sim, "│ SKI      ", who)?;
    let ski: String = line
        .split_once("SKI")
        .map(|(_, rest)| rest.chars().filter(|c| c.is_ascii_hexdigit()).collect())
        .unwrap_or_default();
    (ski.len() == 40)
        .then_some(ski)
        .ok_or_else(|| format!("{who} printed no usable SKI"))
}

fn await_line(sim: &Sim, needle: &str, who: &str) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let log = sim.output();
        if let Some(line) = log.lines().find(|l| l.contains(needle)) {
            return Ok(line.to_string());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("{who} never printed `{needle}`\n{}", sim.output()))
}
