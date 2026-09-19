//! A signaling session whose peer goes silent must close itself.
//!
//! Real failure this guards: a machine changes network (wifi roam, DHCP move,
//! VPN toggle) and the old TCP connection is black-holed rather than reset.
//! Writes keep succeeding into the local socket buffer, so without a
//! dead-peer deadline the session looks alive for minutes and the owner never
//! reconnects. The peer here is a raw WebSocket server that completes the
//! handshake and then never answers anything, which is exactly what a
//! black-holed path looks like from the client side.

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use swarm_core::signal::SignalMessage;
use swarm_stun_client::{KeepAlive, SignalingClient};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Accepts one connection, answers `hello` with a `hello_ack`, then reads
/// and discards everything without ever replying. When `answer_pings` is
/// true it behaves like a healthy server instead and pongs every ping.
async fn spawn_fake_signaling_peer(answer_pings: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        // Consume the client's `hello`.
        ws.next().await.unwrap().unwrap();
        let ack = SignalMessage::HelloAck {
            session_id: "s1".into(),
            observed_addr: "127.0.0.1:1".into(),
            reflector_ports: vec![],
        };
        ws.send(WsMessage::Text(serde_json::to_string(&ack).unwrap()))
            .await
            .unwrap();
        while let Some(Ok(frame)) = ws.next().await {
            if !answer_pings {
                continue;
            }
            if let WsMessage::Text(text) = frame {
                if let Ok(SignalMessage::Ping { seq }) = serde_json::from_str(&text) {
                    let pong = SignalMessage::Pong { seq };
                    if ws
                        .send(WsMessage::Text(serde_json::to_string(&pong).unwrap()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    format!("http://{addr}")
}

const FAST: KeepAlive = KeepAlive {
    ping_interval: Duration::from_millis(50),
    dead_after: Duration::from_millis(200),
};

#[tokio::test]
async fn a_silent_peer_closes_the_session() {
    let base = spawn_fake_signaling_peer(false).await;
    let (_client, mut rx) = SignalingClient::connect_with_keepalive(&base, "tok", "dev", None, FAST)
        .await
        .unwrap();

    // `None` is the documented "session is over, reconnect" signal.
    let closed = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("a silent peer must close the session, not leave it hanging");
    assert!(closed.is_none(), "expected the channel to close, got {closed:?}");
}

#[tokio::test]
async fn a_peer_that_answers_pings_keeps_the_session_open() {
    let base = spawn_fake_signaling_peer(true).await;
    let (_client, mut rx) = SignalingClient::connect_with_keepalive(&base, "tok", "dev", None, FAST)
        .await
        .unwrap();

    // Several times longer than `dead_after`: a healthy session must survive
    // it, so nothing may arrive on the channel — least of all a close.
    let outcome = tokio::time::timeout(Duration::from_millis(1000), rx.recv()).await;
    assert!(
        outcome.is_err(),
        "a healthy session must stay open, but the channel yielded {outcome:?}"
    );
}
