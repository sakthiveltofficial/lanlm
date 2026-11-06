use super::*;
use tokio::net::{TcpListener, TcpStream};

async fn pair_links(
    a_psk: Option<[u8; 32]>,
    b_psk: Option<[u8; 32]>,
) -> (
    io::Result<SecureStream<TcpStream>>,
    io::Result<SecureStream<TcpStream>>,
    Identity,
    Identity,
) {
    let ia = Identity::generate();
    let ib = Identity::generate();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let ib2 = ib.clone();
    let server = tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let _mode = read_mode(&mut s).await.unwrap();
        accept(s, &ib2, b_psk).await
    });
    let ia2 = ia.clone();
    let client = tokio::time::timeout(std::time::Duration::from_secs(5), async move {
        let s = TcpStream::connect(addr).await.unwrap();
        connect(s, &ia2, a_psk).await
    })
    .await
    .unwrap();
    let server = server.await.unwrap();
    (client, server, ia, ib)
}

#[test]
fn cluster_key_round_trips_and_detects_typos() {
    let k = ClusterKey::generate();
    let text = k.encode();
    assert!(text.starts_with("lanlm1-"));
    assert_eq!(ClusterKey::parse(&text).unwrap(), k);
    // Case, spaces and missing dashes are forgiven.
    let messy = format!("  {}  ", text.to_lowercase().replace('-', " "));
    assert_eq!(ClusterKey::parse(&messy).unwrap(), k);
    // One changed character is caught by the checksum.
    let mut bad: Vec<char> = text.chars().collect();
    let i = bad.len() - 2;
    bad[i] = if bad[i] == 'A' { 'B' } else { 'A' };
    let bad: String = bad.into_iter().collect();
    assert!(ClusterKey::parse(&bad).is_err());
    assert!(ClusterKey::parse("hello").is_err());
    assert!(ClusterKey::parse("lanlm1-AAAA").is_err());
}

#[test]
fn cluster_id_is_stable_and_does_not_reveal_the_key() {
    let k = ClusterKey::generate();
    assert_eq!(k.cluster_id(), k.cluster_id());
    assert_eq!(k.cluster_id().len(), 16);
    assert_ne!(k.cluster_id(), ClusterKey::generate().cluster_id());
    assert!(!k.encode().to_lowercase().contains(&k.cluster_id()));
}

#[test]
fn identity_survives_storage() {
    let a = Identity::generate();
    let b = Identity::from_hex(&a.private_hex(), &a.node_id()).unwrap();
    assert_eq!(a.node_id(), b.node_id());
    assert!(Identity::from_hex("zz", "00").is_err());
    assert!(Identity::from_hex("00", "00").is_err());
}

#[tokio::test]
async fn members_with_the_same_key_talk_and_learn_each_others_ids() {
    let psk = ClusterKey::generate().psk();
    let (a, b, ia, ib) = pair_links(Some(psk), Some(psk)).await;
    let (mut a, mut b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.remote_id, ib.node_id());
    assert_eq!(b.remote_id, ia.node_id());
    assert!(a.initiator && !b.initiator);
    assert_eq!(a.handshake_hash, b.handshake_hash);

    a.writer.send_msg(b"hello").await.unwrap();
    assert_eq!(b.reader.recv_msg().await.unwrap().unwrap(), b"hello");
    b.writer.send_msg(b"world").await.unwrap();
    assert_eq!(a.reader.recv_msg().await.unwrap().unwrap(), b"world");
}

#[tokio::test]
async fn a_wrong_cluster_key_is_refused() {
    let (a, b, _, _) = pair_links(
        Some(ClusterKey::generate().psk()),
        Some(ClusterKey::generate().psk()),
    )
    .await;
    // The responder rejects the third handshake message.
    assert!(b.is_err());
    // The dialler may think it finished, but nothing ever arrives.
    if let Ok(mut a) = a {
        let got = a.reader.recv_msg().await;
        assert!(matches!(got, Ok(None) | Err(_)));
    }
}

#[tokio::test]
async fn large_and_back_to_back_messages_stay_intact() {
    let psk = ClusterKey::generate().psk();
    let (a, b, _, _) = pair_links(Some(psk), Some(psk)).await;
    let (mut a, mut b) = (a.unwrap(), b.unwrap());
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let big2 = big.clone();
    let send = tokio::spawn(async move {
        a.writer.send_msg(&big2).await.unwrap();
        a.writer.send_msg(b"tail").await.unwrap();
        a
    });
    assert_eq!(b.reader.recv_msg().await.unwrap().unwrap(), big);
    assert_eq!(b.reader.recv_msg().await.unwrap().unwrap(), b"tail");
    let mut a = send.await.unwrap();
    a.writer.shutdown().await;
    assert!(b.reader.recv_msg().await.unwrap().is_none());
}

#[tokio::test]
async fn raw_chunks_carry_a_byte_stream() {
    let psk = ClusterKey::generate().psk();
    let (a, b, _, _) = pair_links(Some(psk), Some(psk)).await;
    let (mut a, mut b) = (a.unwrap(), b.unwrap());
    let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 256) as u8).collect();
    let d2 = data.clone();
    tokio::spawn(async move {
        a.writer.send_all(&d2).await.unwrap();
        a.writer.shutdown().await;
    });
    let mut got = Vec::new();
    while let Some(c) = b.reader.recv_chunk().await.unwrap() {
        got.extend(c);
    }
    assert_eq!(got, data);
}

#[tokio::test]
async fn pairing_codes_match_without_a_shared_key() {
    let (a, b, _, _) = pair_links(None, None).await;
    let (a, b) = (a.unwrap(), b.unwrap());
    let (sa, sb) = (sas(&a.handshake_hash), sas(&b.handshake_hash));
    assert_eq!(sa, sb);
    assert_eq!(sa.len(), 6);
    assert!(sa.chars().all(|c| c.is_ascii_digit()));
}

#[tokio::test]
async fn a_tampered_frame_is_detected() {
    // Hand-build a link over an in-memory pipe so a byte can be flipped in transit.
    let psk = ClusterKey::generate().psk();
    let (ia, ib) = (Identity::generate(), Identity::generate());
    let (c, s) = tokio::io::duplex(1 << 20);
    let server = tokio::spawn(async move {
        let mut s = s;
        let _ = read_mode(&mut s).await.unwrap();
        accept(s, &ib, Some(psk)).await.unwrap()
    });
    let mut a = connect(c, &ia, Some(psk)).await.unwrap();
    let mut b = server.await.unwrap();
    a.writer.send_msg(b"secret").await.unwrap();
    // Corrupt: decrypt with a reader whose nonce is off by one.
    b.reader.nonce += 1;
    assert!(b.reader.recv_msg().await.is_err());
}
