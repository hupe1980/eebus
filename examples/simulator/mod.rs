//! What the two §14a simulators need and the crate deliberately does not decide: where a
//! device keeps its identity, where it keeps its trust store, and how it talks to a
//! person.
//!
//! None of this is protocol. It is here because a simulator that generated a fresh
//! certificate on every start would be useless — the whole point of pairing is that a
//! peer stays paired — and because the parts an installer touches, the SKI and the QR
//! code, are the parts a worked example most needs to show.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};

use eebus::cert::{self, CertParams, Identity};
use eebus::mdns::Mdns;
use eebus::runtime::{TrustStore, TrustedPeer};
use eebus::ship::{ShipId, ShipQr, ShipTxtRecord, Ski};

/// Where a simulated device keeps its state.
///
/// A real device would use its own flash; the point of naming a directory is that the
/// files are the same shape either way — a private key, and a list of trusted SKIs.
#[derive(Clone, Debug)]
pub struct DeviceStore {
    dir: PathBuf,
}

impl DeviceStore {
    /// Opens (and creates) a state directory.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join("identity.key.pem")
    }

    fn trust_path(&self) -> PathBuf {
        self.dir.join("trust.json")
    }

    /// The device's identity, generated on first run and reused after that.
    ///
    /// SHIP §12.1.1 is the reason this is persisted rather than regenerated: the SKI is
    /// what a peer trusted, and on a real device it is printed on the label. A node that
    /// came back with a new one would have to be paired again by hand every time it
    /// restarted.
    pub fn identity(&self, ship_id: &str) -> Result<Identity, Box<dyn std::error::Error>> {
        let path = self.key_path();
        if let Ok(pem) = std::fs::read_to_string(&path) {
            let key = cert::key_from_pem(&pem)?;
            return Ok(cert::self_signed_with(CertParams::new(ship_id), key)?);
        }

        let identity = cert::self_signed(CertParams::new(ship_id))?;
        write_private(&path, identity.key_pem().as_bytes())?;
        println!("generated a new identity in {}", path.display());
        Ok(identity)
    }

    /// The trust store, read back from disk.
    pub fn trust(&self) -> TrustStore {
        match std::fs::read_to_string(self.trust_path()) {
            Ok(json) => TrustStore::from_json(&json).unwrap_or_else(|error| {
                eprintln!("trust.json is unreadable ({error}); starting with nothing trusted");
                TrustStore::new()
            }),
            Err(_) => TrustStore::new(),
        }
    }

    /// Writes the trust store back. Call it after every change, not at shutdown: a device
    /// that loses power between a pairing and its own exit has lost the pairing.
    pub fn save_trust(&self, trust: &TrustStore) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::write(self.trust_path(), trust.to_json()?)?;
        Ok(())
    }

    /// The other half of an EEBUS reset: removing what was persisted.
    ///
    /// [`eebus::runtime::Node::eebus_reset`] empties the in-memory store; this removes the
    /// file behind it and the identity, so the device comes back as it left the factory.
    /// A real device restores the *original* key rather than deleting it, because the SKI
    /// on its label has to keep being true — this one has no label.
    pub fn factory_reset(&self) -> std::io::Result<()> {
        for path in [self.trust_path(), self.key_path()] {
            match std::fs::remove_file(&path) {
                Ok(()) => println!("removed {}", path.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// Prints what an installer would find on the device's label and screen.
///
/// The SKI in groups of four is what SHIP §13.4.5 asks a user to compare; the QR payload
/// is what the installation-process specification asks a device to display, and scanning
/// it is what replaces the comparing.
pub fn show_identity(ship_id: &ShipId, ski: Ski, port: u16) {
    let qr = ShipQr::new(ski, ship_id.clone());
    println!("┌─ this device ─────────────────────────────────────────────");
    println!("│ SHIP ID  {}", ship_id.as_str());
    println!("│ SKI      {}", ski.to_display_string());
    println!("│ port     {port}");
    println!("│ QR       {}", qr.to_payload());
    println!("└───────────────────────────────────────────────────────────");
}

/// Starts announcing the node as `_ship._tcp`, with the TXT record set the installation
/// process specification asks for.
pub fn announce(
    record: &ShipTxtRecord,
    instance: &str,
    port: u16,
) -> Result<Mdns, Box<dyn std::error::Error>> {
    let mut mdns = Mdns::new()?;
    let addresses = local_addresses();
    mdns.announce(instance, record, port, &addresses)?;
    println!("announcing {instance} on {SERVICE} at {addresses:?}:{port}\n");
    Ok(mdns)
}

const SERVICE: &str = eebus::ship::SERVICE_TYPE;

/// The addresses to announce.
///
/// A real device announces the address of the interface it is reachable on. These
/// simulators are usually run on one machine or two on the same subnet, so loopback plus
/// whatever the host resolves to covers both; `--address` overrides it when neither is
/// what you meant.
fn local_addresses() -> Vec<std::net::IpAddr> {
    let mut addresses = vec![std::net::IpAddr::from([127, 0, 0, 1])];
    if let Some(extra) = std::env::var_os("EEBUS_ADDRESS")
        && let Some(parsed) = extra.to_str().and_then(|text| text.parse().ok())
    {
        addresses.push(parsed);
    }
    addresses
}

/// A peer approved on the command line, or by a user pressing a button.
pub fn approve(trust: &TrustStore, store: &DeviceStore, peer: TrustedPeer) {
    let ski = peer.ski;
    trust.remember(peer);
    if let Err(error) = store.save_trust(trust) {
        eprintln!("could not persist the trust store: {error}");
    }
    println!("trusting {}", ski.to_display_string());
}

/// A very small argument reader: `--key value` and `--flag`.
pub struct Args(Vec<String>);

impl Args {
    /// Reads the process arguments.
    pub fn from_env() -> Self {
        Self(std::env::args().skip(1).collect())
    }

    /// The value after `--name`, if it is there.
    pub fn value(&self, name: &str) -> Option<&str> {
        let index = self.0.iter().position(|arg| arg == name)?;
        self.0.get(index + 1).map(String::as_str)
    }

    /// Whether `--name` was given.
    pub fn flag(&self, name: &str) -> bool {
        self.0.iter().any(|arg| arg == name)
    }

    /// The value after `--name`, or a default.
    pub fn value_or<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        self.value(name).unwrap_or(default)
    }
}
