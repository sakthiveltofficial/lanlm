//! Identity, cluster keys and the encrypted, mutually authenticated link between nodes.
//!
//! Every node has a long-lived X25519 identity (its public key *is* its node id). Nodes that
//! belong to one cluster share a 256-bit cluster key. Two nodes talk over the Noise
//! `XXpsk3` pattern keyed with that cluster key, so:
//!
//! * a device without the key cannot complete the handshake (it learns nothing and gets nothing),
//! * traffic is encrypted and tamper-proof, with forward secrecy (ephemeral keys),
//! * each side learns the other's node id from the handshake, not from what it claims.
//!
//! A second, key-less pattern (`XX`) is used only for *pairing*: both screens show a short
//! code derived from the handshake, which matches only when nobody is in the middle.

use std::io;
use std::sync::Arc;

use data_encoding::BASE32_NOPAD;
use rand::RngCore;
use sha2::{Digest, Sha256};
use snow::{Builder, StatelessTransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

/// Noise pattern for cluster members: static keys on both sides plus the cluster key.
pub const PATTERN_CLUSTER: &str = "Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s";
/// Noise pattern for pairing: no shared secret yet, so the humans compare a code.
pub const PATTERN_PAIR: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// First byte on the wire: which handshake follows.
pub const MODE_CLUSTER: u8 = 0;
pub const MODE_PAIR: u8 = 1;

const TAG_LEN: usize = 16;
/// Largest plaintext that fits in one Noise message.
pub const MAX_PLAIN: usize = 65535 - TAG_LEN;
/// Largest control message we accept (a guard against a peer that lies about its length).
pub const MAX_MSG: usize = 8 * 1024 * 1024;

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

// ───────────────────────────── identity ─────────────────────────────

/// A node's long-lived key pair. The public half, in hex, is the node id.
#[derive(Clone)]
pub struct Identity {
    private: Vec<u8>,
    public: Vec<u8>,
}

impl Identity {
    pub fn generate() -> Identity {
        let kp = Builder::new(PATTERN_CLUSTER.parse().expect("pattern"))
            .generate_keypair()
            .expect("keypair");
        Identity {
            private: kp.private,
            public: kp.public,
        }
    }

    /// Rebuilds an identity from the keys stored on disk (both halves, in hex).
    pub fn from_hex(private_hex: &str, public_hex: &str) -> Result<Identity, String> {
        let private = hex::decode(private_hex.trim()).map_err(|e| e.to_string())?;
        let public = hex::decode(public_hex.trim()).map_err(|e| e.to_string())?;
        if private.len() != 32 || public.len() != 32 {
            return Err("identity keys must be 32 bytes".into());
        }
        Ok(Identity { private, public })
    }

    pub fn private_hex(&self) -> String {
        hex::encode(&self.private)
    }

    /// The node id: the public key in hex.
    pub fn node_id(&self) -> String {
        hex::encode(&self.public)
    }
}

// ───────────────────────────── cluster key ─────────────────────────────

/// The secret every member of one cluster shares.
#[derive(Clone, PartialEq, Eq)]
pub struct ClusterKey([u8; 32]);

impl std::fmt::Debug for ClusterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClusterKey(id={})", self.cluster_id())
    }
}

impl ClusterKey {
    pub fn generate() -> ClusterKey {
        let mut k = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut k);
        ClusterKey(k)
    }

    pub fn from_bytes(b: [u8; 32]) -> ClusterKey {
        ClusterKey(b)
    }

    /// The pre-shared key fed to Noise, domain-separated from every other use of the secret.
    pub fn psk(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"lanlm-cluster-psk-v1");
        h.update(self.0);
        h.finalize().into()
    }

    /// A public label for the cluster. It cannot be turned back into the key, so it is safe to
    /// broadcast so that devices of the same cluster can find each other.
    pub fn cluster_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"lanlm-cluster-id-v1");
        h.update(self.0);
        hex::encode(&h.finalize()[..8])
    }

    /// What people copy between PCs: `lanlm1-XXXX-XXXX-...` (key plus a 2-byte typo check).
    pub fn encode(&self) -> String {
        let mut raw = self.0.to_vec();
        raw.extend_from_slice(&check_bytes(&self.0));
        let text = BASE32_NOPAD.encode(&raw);
        let groups: Vec<&str> = text
            .as_bytes()
            .chunks(5)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!("lanlm1-{}", groups.join("-"))
    }

    pub fn parse(text: &str) -> Result<ClusterKey, String> {
        let t: String = text
            .trim()
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect::<String>()
            .to_ascii_uppercase();
        let body = t
            .strip_prefix("LANLM1")
            .ok_or("That is not a LanLM cluster key (it should start with lanlm1-)")?;
        let raw = BASE32_NOPAD
            .decode(body.as_bytes())
            .map_err(|_| "The cluster key has a character that does not belong in it")?;
        if raw.len() != 34 {
            return Err("The cluster key is the wrong length. Copy all of it.".into());
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&raw[..32]);
        if raw[32..] != check_bytes(&k) {
            return Err("The cluster key has a typo (checksum failed)".into());
        }
        Ok(ClusterKey(k))
    }
}

fn check_bytes(k: &[u8; 32]) -> [u8; 2] {
    let mut h = Sha256::new();
    h.update(b"lanlm-key-check-v1");
    h.update(k);
    let d = h.finalize();
    [d[0], d[1]]
}

/// The six digits both screens show during pairing.
pub fn sas(handshake_hash: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"lanlm-sas-v1");
    h.update(handshake_hash);
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{n:06}")
}

// ───────────────────────────── framing ─────────────────────────────

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 2];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u16::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> io::Result<()> {
    debug_assert!(data.len() <= u16::MAX as usize);
    let mut out = Vec::with_capacity(2 + data.len());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
    w.write_all(&out).await
}

// ───────────────────────────── secure stream ─────────────────────────────

/// Receiving half of an encrypted link.
pub struct SecureReader<R> {
    r: R,
    t: Arc<StatelessTransportState>,
    nonce: u64,
    /// Bytes already decrypted but not yet handed out (used by `recv_msg`).
    buf: Vec<u8>,
}

/// Sending half of an encrypted link.
pub struct SecureWriter<W> {
    w: W,
    t: Arc<StatelessTransportState>,
    nonce: u64,
}

/// An encrypted, authenticated link to one peer.
pub struct SecureStream<S> {
    pub reader: SecureReader<ReadHalf<S>>,
    pub writer: SecureWriter<WriteHalf<S>>,
    /// The peer's node id, taken from the handshake.
    pub remote_id: String,
    /// Binds this exact link; used to build the pairing code.
    pub handshake_hash: Vec<u8>,
    /// True on the side that opened the connection.
    pub initiator: bool,
}

impl<R: AsyncRead + Unpin> SecureReader<R> {
    /// Next decrypted chunk, or `None` when the peer closed the link cleanly.
    pub async fn recv_chunk(&mut self) -> io::Result<Option<Vec<u8>>> {
        if !self.buf.is_empty() {
            return Ok(Some(std::mem::take(&mut self.buf)));
        }
        self.read_one().await
    }

    async fn read_one(&mut self) -> io::Result<Option<Vec<u8>>> {
        let Some(frame) = read_frame(&mut self.r).await? else {
            return Ok(None);
        };
        let mut out = vec![0u8; frame.len()];
        let n = self
            .t
            .read_message(self.nonce, &frame, &mut out)
            .map_err(|e| err(format!("decrypt failed: {e}")))?;
        self.nonce += 1;
        out.truncate(n);
        Ok(Some(out))
    }

    /// Next whole message (length-prefixed), or `None` on a clean close between messages.
    pub async fn recv_msg(&mut self) -> io::Result<Option<Vec<u8>>> {
        while self.buf.len() < 4 {
            match self.read_one().await? {
                Some(c) => self.buf.extend_from_slice(&c),
                None if self.buf.is_empty() => return Ok(None),
                None => return Err(err("link closed in the middle of a message")),
            }
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_MSG {
            return Err(err("message too large"));
        }
        while self.buf.len() < 4 + len {
            match self.read_one().await? {
                Some(c) => self.buf.extend_from_slice(&c),
                None => return Err(err("link closed in the middle of a message")),
            }
        }
        let rest = self.buf.split_off(4 + len);
        let msg = self.buf[4..].to_vec();
        self.buf = rest;
        Ok(Some(msg))
    }
}

impl<W: AsyncWrite + Unpin> SecureWriter<W> {
    /// Encrypts and sends `data`, splitting it across frames as needed.
    pub async fn send_all(&mut self, data: &[u8]) -> io::Result<()> {
        for chunk in data.chunks(MAX_PLAIN) {
            let mut out = vec![0u8; chunk.len() + TAG_LEN];
            let n = self
                .t
                .write_message(self.nonce, chunk, &mut out)
                .map_err(|e| err(format!("encrypt failed: {e}")))?;
            self.nonce += 1;
            write_frame(&mut self.w, &out[..n]).await?;
        }
        Ok(())
    }

    /// Sends one length-prefixed message.
    pub async fn send_msg(&mut self, data: &[u8]) -> io::Result<()> {
        if data.len() > MAX_MSG {
            return Err(err("message too large"));
        }
        let mut buf = Vec::with_capacity(4 + data.len());
        buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
        buf.extend_from_slice(data);
        self.send_all(&buf).await
    }

    pub async fn shutdown(&mut self) {
        let _ = self.w.shutdown().await;
    }
}

fn finish<S: AsyncRead + AsyncWrite>(
    stream: S,
    hs: snow::HandshakeState,
    initiator: bool,
) -> io::Result<SecureStream<S>> {
    let remote = hs
        .get_remote_static()
        .ok_or_else(|| err("the peer sent no identity"))?
        .to_vec();
    let hash = hs.get_handshake_hash().to_vec();
    let t = Arc::new(
        hs.into_stateless_transport_mode()
            .map_err(|e| err(e.to_string()))?,
    );
    let (r, w) = tokio::io::split(stream);
    Ok(SecureStream {
        reader: SecureReader {
            r,
            t: t.clone(),
            nonce: 0,
            buf: Vec::new(),
        },
        writer: SecureWriter { w, t, nonce: 0 },
        remote_id: hex::encode(remote),
        handshake_hash: hash,
        initiator,
    })
}

fn builder<'a>(key: Option<&'a [u8; 32]>, id: &'a Identity) -> Builder<'a> {
    let pattern = if key.is_some() {
        PATTERN_CLUSTER
    } else {
        PATTERN_PAIR
    };
    let b = Builder::new(pattern.parse().expect("pattern")).local_private_key(&id.private);
    match key {
        Some(k) => b.psk(3, k),
        None => b,
    }
}

/// Opens the handshake as the side that dialled. `psk` is the cluster key's psk, or `None` to pair.
pub async fn connect<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    id: &Identity,
    psk: Option<[u8; 32]>,
) -> io::Result<SecureStream<S>> {
    stream
        .write_all(&[if psk.is_some() {
            MODE_CLUSTER
        } else {
            MODE_PAIR
        }])
        .await?;
    let mut hs = builder(psk.as_ref(), id)
        .build_initiator()
        .map_err(|e| err(e.to_string()))?;
    let mut buf = vec![0u8; 65535];
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|e| err(e.to_string()))?;
    write_frame(&mut stream, &buf[..n]).await?;
    let f = read_frame(&mut stream)
        .await?
        .ok_or_else(|| err("the peer closed the link during the handshake"))?;
    hs.read_message(&f, &mut buf)
        .map_err(|e| err(format!("handshake failed: {e}")))?;
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|e| err(e.to_string()))?;
    write_frame(&mut stream, &buf[..n]).await?;
    finish(stream, hs, true)
}

/// Reads the mode byte of an incoming connection.
pub async fn read_mode<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<u8> {
    let mut b = [0u8; 1];
    stream.read_exact(&mut b).await?;
    Ok(b[0])
}

/// Completes the handshake as the side that accepted, after `read_mode`.
pub async fn accept<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    id: &Identity,
    psk: Option<[u8; 32]>,
) -> io::Result<SecureStream<S>> {
    let mut hs = builder(psk.as_ref(), id)
        .build_responder()
        .map_err(|e| err(e.to_string()))?;
    let mut buf = vec![0u8; 65535];
    let f = read_frame(&mut stream)
        .await?
        .ok_or_else(|| err("the peer closed the link during the handshake"))?;
    hs.read_message(&f, &mut buf)
        .map_err(|e| err(format!("handshake failed: {e}")))?;
    let n = hs
        .write_message(&[], &mut buf)
        .map_err(|e| err(e.to_string()))?;
    write_frame(&mut stream, &buf[..n]).await?;
    let f = read_frame(&mut stream)
        .await?
        .ok_or_else(|| err("the peer closed the link during the handshake"))?;
    // With the wrong cluster key this is where it fails: the third message is bound to the key.
    hs.read_message(&f, &mut buf)
        .map_err(|e| err(format!("handshake failed (wrong cluster key?): {e}")))?;
    finish(stream, hs, false)
}

#[cfg(test)]
mod tests;
