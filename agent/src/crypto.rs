//! End-to-end session crypto shared with the browser viewer (see docs/PROTOCOL.md).
//!
//! X25519 ephemeral key exchange, authenticated by the device's Ed25519 identity,
//! HKDF-SHA256 into one AES-256-GCM key per direction with counter nonces.

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use anyhow::{Result, anyhow, bail};
use ed25519_dalek::{Signer, SigningKey};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS random generator unavailable");
    b
}

/// 9-digit id like RustDesk's, never starting with 0.
pub fn random_device_id() -> String {
    let n = u64::from_le_bytes(random_bytes::<8>()) % 900_000_000 + 100_000_000;
    n.to_string()
}

/// Unambiguous alphabet for passwords read aloud or typed from a screen.
pub fn random_password(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    random_bytes::<32>()[..len].iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

pub fn transcript(sid: &str, viewer_epk: &[u8; 32], agent_epk: &[u8; 32]) -> Vec<u8> {
    let mut t = b"redesk-kx-v1".to_vec();
    t.extend_from_slice(sid.as_bytes());
    t.extend_from_slice(viewer_epk);
    t.extend_from_slice(agent_epk);
    t
}

pub struct Handshake {
    pub agent_epk: [u8; 32],
    pub signature: [u8; 64],
    pub channel: Channel,
}

/// Agent side of the key exchange: answer the viewer's ephemeral key.
pub fn agent_handshake(identity: &SigningKey, sid: &str, viewer_epk: [u8; 32]) -> Result<Handshake> {
    let secret = StaticSecret::from(random_bytes::<32>());
    let agent_epk = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(viewer_epk));
    if !shared.was_contributory() {
        bail!("viewer sent a low-order public key");
    }
    let t = transcript(sid, &viewer_epk, &agent_epk);
    let signature = identity.sign(&t).to_bytes();
    let salt = Sha256::digest(&t);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut v2a = [0u8; 32];
    let mut a2v = [0u8; 32];
    hk.expand(b"redesk v1 viewer->agent", &mut v2a).map_err(|_| anyhow!("hkdf"))?;
    hk.expand(b"redesk v1 agent->viewer", &mut a2v).map_err(|_| anyhow!("hkdf"))?;
    Ok(Handshake {
        agent_epk,
        signature,
        channel: Channel {
            send: Aes256Gcm::new_from_slice(&a2v)?,
            recv: Aes256Gcm::new_from_slice(&v2a)?,
            send_ctr: 0,
            recv_ctr: 0,
        },
    })
}

/// Viewer side of the key exchange (the native viewer; the browser does the same in crypto.ts).
pub struct ViewerKeys {
    secret: StaticSecret,
    pub epk: [u8; 32],
}

impl ViewerKeys {
    pub fn new() -> Self {
        let secret = StaticSecret::from(random_bytes::<32>());
        let epk = PublicKey::from(&secret).to_bytes();
        Self { secret, epk }
    }

    /// Verifies the agent's signature with the device key the server bound at registration,
    /// then derives the session keys. A relay or server that swaps keys fails here.
    pub fn finish(self, sid: &str, agent_epk: [u8; 32], signature: &[u8; 64], device_key: &[u8; 32]) -> Result<Channel> {
        use ed25519_dalek::Verifier;
        let t = transcript(sid, &self.epk, &agent_epk);
        ed25519_dalek::VerifyingKey::from_bytes(device_key)?
            .verify(&t, &ed25519_dalek::Signature::from_bytes(signature))
            .map_err(|_| anyhow!("ลายเซ็นของเครื่องปลายทางไม่ถูกต้อง — อาจมีการดักกลางทาง"))?;
        let shared = self.secret.diffie_hellman(&PublicKey::from(agent_epk));
        if !shared.was_contributory() {
            bail!("agent sent a low-order public key");
        }
        let hk = Hkdf::<Sha256>::new(Some(&Sha256::digest(&t)), shared.as_bytes());
        let (mut v2a, mut a2v) = ([0u8; 32], [0u8; 32]);
        hk.expand(b"redesk v1 viewer->agent", &mut v2a).map_err(|_| anyhow!("hkdf"))?;
        hk.expand(b"redesk v1 agent->viewer", &mut a2v).map_err(|_| anyhow!("hkdf"))?;
        Ok(Channel {
            send: Aes256Gcm::new_from_slice(&v2a)?,
            recv: Aes256Gcm::new_from_slice(&a2v)?,
            send_ctr: 0,
            recv_ctr: 0,
        })
    }
}

impl Default for ViewerKeys {
    fn default() -> Self {
        Self::new()
    }
}

/// Ordered, authenticated channel over a reliable transport. A replayed, dropped
/// or reordered message fails decryption because the nonce counter is implicit.
pub struct Channel {
    send: Aes256Gcm,
    recv: Aes256Gcm,
    send_ctr: u64,
    recv_ctr: u64,
}

fn nonce(ctr: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&ctr.to_be_bytes());
    n
}

impl Channel {
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let n = nonce(self.send_ctr);
        self.send_ctr += 1;
        self.send.encrypt(Nonce::from_slice(&n), plaintext).map_err(|_| anyhow!("encrypt failed"))
    }

    pub fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let n = nonce(self.recv_ctr);
        self.recv_ctr += 1;
        self.recv.decrypt(Nonce::from_slice(&n), ciphertext).map_err(|_| anyhow!("decrypt failed (tampered or out of order)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_viewer_interop() {
        let identity = SigningKey::from_bytes(&random_bytes::<32>());
        let keys = ViewerKeys::new();
        let mut agent = agent_handshake(&identity, "s1", keys.epk).unwrap();
        let mut viewer = keys.finish("s1", agent.agent_epk, &agent.signature, identity.verifying_key().as_bytes()).unwrap();
        assert_eq!(agent.channel.open(&viewer.seal(b"hi").unwrap()).unwrap(), b"hi");
        assert_eq!(viewer.open(&agent.channel.seal(b"yo").unwrap()).unwrap(), b"yo");
        // A different device key (e.g. an impostor) must be rejected.
        let other = SigningKey::from_bytes(&random_bytes::<32>());
        let keys = ViewerKeys::new();
        let hs = agent_handshake(&identity, "s2", keys.epk).unwrap();
        assert!(keys.finish("s2", hs.agent_epk, &hs.signature, other.verifying_key().as_bytes()).is_err());
    }

    #[test]
    fn handshake_round_trip() {
        let identity = SigningKey::from_bytes(&random_bytes::<32>());
        let viewer_secret = StaticSecret::from(random_bytes::<32>());
        let viewer_epk = PublicKey::from(&viewer_secret).to_bytes();
        let mut hs = agent_handshake(&identity, "sid", viewer_epk).unwrap();

        // Viewer side, mirroring web/src/lib/crypto.ts.
        let t = transcript("sid", &viewer_epk, &hs.agent_epk);
        use ed25519_dalek::Verifier;
        identity.verifying_key().verify(&t, &ed25519_dalek::Signature::from_bytes(&hs.signature)).unwrap();
        let shared = viewer_secret.diffie_hellman(&PublicKey::from(hs.agent_epk));
        let hk = Hkdf::<Sha256>::new(Some(&Sha256::digest(&t)), shared.as_bytes());
        let (mut v2a, mut a2v) = ([0u8; 32], [0u8; 32]);
        hk.expand(b"redesk v1 viewer->agent", &mut v2a).unwrap();
        hk.expand(b"redesk v1 agent->viewer", &mut a2v).unwrap();
        let mut viewer = Channel {
            send: Aes256Gcm::new_from_slice(&v2a).unwrap(),
            recv: Aes256Gcm::new_from_slice(&a2v).unwrap(),
            send_ctr: 0,
            recv_ctr: 0,
        };

        let c = viewer.seal(b"hello agent").unwrap();
        assert_eq!(hs.channel.open(&c).unwrap(), b"hello agent");
        let c1 = hs.channel.seal(b"one").unwrap();
        let c2 = hs.channel.seal(b"two").unwrap();
        assert!(viewer.open(&c2).is_err(), "out-of-order message must be rejected");
        let _ = c1;
    }
}
