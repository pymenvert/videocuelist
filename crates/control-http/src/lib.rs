//! # conduite-control-http
//!
//! Serveur web de Conduite — voir docs/INTERFACES.md (§ control-http + webui,
//! normatif) :
//!
//! - `GET /` et `/assets/*` : web UI embarquée (vanilla, aucun toolchain) ;
//! - `GET /ws` : WebSocket — `hello` (état complet), relais des événements,
//!   trames `dyn` (runtime) throttlées à 10 Hz, commandes entrantes ;
//! - `GET /preview.mjpeg` / `GET /preview-b.mjpeg` : préviews program /
//!   standby en `multipart/x-mixed-replace` ;
//! - `GET /thumb/{id}.jpg` : vignettes du cache.
//!
//! Le serveur vit sur son propre thread (runtime tokio dédié) : la boucle
//! de rendu d'`app` ne dépend jamais de lui.

pub mod assets;
mod server;

pub use server::{HttpDeps, HttpServer, HttpServerHandle};

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use tokio::sync::{broadcast, watch};
    use tokio_tungstenite::tungstenite::Message as TgMessage;

    use conduite_core::{Command, CueNumber, Source};

    use super::*;

    /// Banc de test : serveur démarré sur un port éphémère + tous les
    /// émetteurs gardés vivants côté test.
    struct Harness {
        handle: HttpServerHandle,
        cmd_rx: crossbeam_channel::Receiver<(Source, Command)>,
        state_tx: watch::Sender<Value>,
        events_tx: broadcast::Sender<Value>,
        preview_tx: broadcast::Sender<Bytes>,
        preview_b_tx: broadcast::Sender<Bytes>,
        thumb_dir: PathBuf,
    }

    fn harness(name: &str) -> Harness {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (state_tx, state_rx) = watch::channel(json!({
            "show": { "name": "test-show" },
            "runtime": { "progress": 0.0, "mode": "edit" }
        }));
        let (events_tx, events_rx) = broadcast::channel(64);
        let (preview_tx, preview_rx) = broadcast::channel(8);
        let (preview_b_tx, preview_b_rx) = broadcast::channel(8);
        let thumb_dir = std::env::temp_dir().join(format!(
            "conduite-http-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&thumb_dir);
        std::fs::create_dir_all(&thumb_dir).expect("mkdir vignettes");

        let deps = HttpDeps {
            cmd_tx,
            state_rx,
            events_rx,
            preview_rx,
            preview_b_rx,
            thumb_dir: thumb_dir.clone(),
        };
        let handle = HttpServer::spawn("127.0.0.1:0".parse().expect("addr"), deps).expect("spawn");
        Harness {
            handle,
            cmd_rx,
            state_tx,
            events_tx,
            preview_tx,
            preview_b_tx,
            thumb_dir,
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.thumb_dir);
        }
    }

    /// Requête HTTP brute (Connection: close) — lit toute la réponse.
    fn http_get(h: &Harness, path: &str) -> (String, Vec<u8>) {
        let mut sock = std::net::TcpStream::connect(h.handle.local_addr()).expect("connect");
        sock.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
        write!(
            sock,
            "GET {path} HTTP/1.1\r\nHost: conduite\r\nConnection: close\r\n\r\n"
        )
        .expect("write");
        let mut buf = Vec::new();
        sock.read_to_end(&mut buf).expect("read");
        let split = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("fin des en-têtes");
        let head = String::from_utf8_lossy(&buf[..split]).to_string();
        (head, buf[split + 4..].to_vec())
    }

    // -------------------------------------------------------------- assets

    #[test]
    fn assets_lookup() {
        let (ct, body) = assets::asset("app.js").expect("app.js");
        assert!(ct.starts_with("application/javascript"));
        assert!(body.contains("Conduite"));
        let (ct, body) = assets::asset("style.css").expect("style.css");
        assert!(ct.starts_with("text/css"));
        assert!(body.contains("--bg"));
        let (ct, _) = assets::asset("ws.js").expect("ws.js");
        assert!(ct.starts_with("application/javascript"));
        assert!(assets::asset("inconnu.js").is_none());
        assert!(assets::asset("../secret").is_none());
        assert!(assets::INDEX_HTML.contains("app.js"));
        assert!(assets::INDEX_HTML.contains("style.css"));
    }

    #[test]
    fn index_served_over_http() {
        let h = harness("index");
        let (head, body) = http_get(&h, "/");
        assert!(head.starts_with("HTTP/1.1 200"), "en-têtes : {head}");
        assert!(head.to_lowercase().contains("text/html"));
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("CONDUITE"), "l'index doit être la webui");
        assert!(html.contains("/assets/app.js"));
    }

    #[test]
    fn assets_route_serves_css_and_404() {
        let h = harness("assets");
        let (head, body) = http_get(&h, "/assets/style.css");
        assert!(head.starts_with("HTTP/1.1 200"), "en-têtes : {head}");
        assert!(head.to_lowercase().contains("text/css"));
        assert!(String::from_utf8_lossy(&body).contains("--accent"));
        let (head, _) = http_get(&h, "/assets/nimporte.quoi");
        assert!(head.starts_with("HTTP/1.1 404"), "en-têtes : {head}");
    }

    // -------------------------------------------------------------- thumbs

    #[test]
    fn thumb_served_and_missing_is_404() {
        let h = harness("thumb");
        std::fs::write(h.thumb_dir.join("7.jpg"), b"JPEGDATA").expect("write vignette");

        let (head, body) = http_get(&h, "/thumb/7.jpg");
        assert!(head.starts_with("HTTP/1.1 200"), "en-têtes : {head}");
        assert!(head.to_lowercase().contains("image/jpeg"));
        assert_eq!(body, b"JPEGDATA");

        let (head, _) = http_get(&h, "/thumb/999.jpg");
        assert!(head.starts_with("HTTP/1.1 404"));
        // Pas un id numérique => 404 (jamais de chemin arbitraire).
        let (head, _) = http_get(&h, "/thumb/evil.txt");
        assert!(head.starts_with("HTTP/1.1 404"));
    }

    // ----------------------------------------------------------- WebSocket

    async fn ws_connect(
        h: &Harness,
    ) -> tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    > {
        let url = format!("ws://{}/ws", h.handle.local_addr());
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connexion WS");
        ws
    }

    async fn next_json(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("timeout WS")
                .expect("flux WS terminé")
                .expect("erreur WS");
            if let TgMessage::Text(t) = msg {
                return serde_json::from_str(t.as_str()).expect("JSON WS");
            }
        }
    }

    #[tokio::test]
    async fn ws_hello_then_cmd_relay_and_pong() {
        let h = harness("ws-hello");
        let mut ws = ws_connect(&h).await;

        // 1. hello avec l'état complet courant.
        let hello = next_json(&mut ws).await;
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["state"]["show"]["name"], "test-show");

        // 2. commande relayée sur le canal avec Source::Ui.
        ws.send(TgMessage::Text(r#"{"type":"cmd","cmd":{"cmd":"cue_go"}}"#.into()))
            .await
            .expect("envoi cmd");
        let (src, cmd) = h
            .cmd_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("commande relayée");
        assert_eq!(src, Source::Ui);
        assert_eq!(cmd, Command::CueGo);

        // 3. commande plus riche (goto) : le JSON Command est le contrat core.
        ws.send(TgMessage::Text(
            r#"{"type":"cmd","cmd":{"cmd":"cue_goto","cue":12500}}"#.into(),
        ))
        .await
        .expect("envoi goto");
        let (_, cmd) = h
            .cmd_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("goto relayée");
        assert_eq!(cmd, Command::CueGoto { cue: CueNumber(12500) });

        // 4. ping → pong.
        ws.send(TgMessage::Text(r#"{"type":"ping"}"#.into()))
            .await
            .expect("envoi ping");
        let pong = next_json(&mut ws).await;
        assert_eq!(pong["type"], "pong");
    }

    #[tokio::test]
    async fn ws_invalid_cmd_is_ignored_connection_survives() {
        let h = harness("ws-invalid");
        let mut ws = ws_connect(&h).await;
        let _ = next_json(&mut ws).await; // hello

        // Commande inconnue, JSON cassé, type inconnu : tout est ignoré.
        for bad in [
            r#"{"type":"cmd","cmd":{"cmd":"self_destruct"}}"#,
            r#"{"type":"cmd"}"#,
            r#"pas du json"#,
            r#"{"type":"mystere"}"#,
        ] {
            ws.send(TgMessage::Text(bad.into())).await.expect("envoi");
        }
        // La connexion répond toujours.
        ws.send(TgMessage::Text(r#"{"type":"ping"}"#.into()))
            .await
            .expect("envoi ping");
        let pong = next_json(&mut ws).await;
        assert_eq!(pong["type"], "pong");
        assert!(
            h.cmd_rx.try_recv().is_err(),
            "aucune commande ne doit avoir été relayée"
        );
    }

    #[tokio::test]
    async fn ws_relays_events() {
        let h = harness("ws-events");
        let mut ws = ws_connect(&h).await;
        let _ = next_json(&mut ws).await; // hello

        h.events_tx
            .send(json!({ "type": "cue_changed", "active": 2500 }))
            .expect("émission event");
        let msg = next_json(&mut ws).await;
        assert_eq!(msg["type"], "event");
        assert_eq!(msg["event"]["type"], "cue_changed");
        assert_eq!(msg["event"]["active"], 2500);
    }

    #[tokio::test]
    async fn ws_sends_dyn_on_state_change() {
        let h = harness("ws-dyn");
        let mut ws = ws_connect(&h).await;
        let _ = next_json(&mut ws).await; // hello

        h.state_tx
            .send(json!({
                "show": { "name": "test-show" },
                "runtime": { "progress": 0.5, "mode": "edit" }
            }))
            .expect("maj état");

        // Une trame dyn (runtime seul) doit arriver dans la fenêtre 10 Hz.
        let msg = next_json(&mut ws).await;
        assert_eq!(msg["type"], "dyn");
        assert_eq!(msg["runtime"]["progress"], 0.5);
        assert!(msg.get("state").is_none(), "dyn ne renvoie pas le show complet");
    }

    // --------------------------------------------------------------- MJPEG

    /// Lit un flux MJPEG jusqu'à voir une frame `marker` (ou échoue à 5 s).
    async fn assert_mjpeg_delivers(
        h: &Harness,
        path: &str,
        tx: broadcast::Sender<Bytes>,
        marker: &'static [u8],
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut sock = tokio::net::TcpStream::connect(h.handle.local_addr())
            .await
            .expect("connect");
        sock.write_all(format!("GET {path} HTTP/1.1\r\nHost: conduite\r\n\r\n").as_bytes())
            .await
            .expect("write");

        // Le producteur pousse des frames pendant que le client lit.
        let producer = tokio::spawn(async move {
            for _ in 0..100 {
                let _ = tx.send(Bytes::from_static(marker));
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        let mut buf = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let mut chunk = [0u8; 4096];
            let n = tokio::time::timeout_at(deadline, sock.read(&mut chunk))
                .await
                .expect("timeout MJPEG")
                .expect("lecture");
            assert!(n > 0, "flux MJPEG fermé prématurément");
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if text.contains("--frame") && text.contains(&String::from_utf8_lossy(marker).to_string()) {
                assert!(text.contains("multipart/x-mixed-replace"));
                assert!(text.contains(&format!("Content-Length: {}", marker.len())));
                break;
            }
        }
        producer.abort();
    }

    #[tokio::test]
    async fn mjpeg_streams_program_frames() {
        let h = harness("mjpeg");
        assert_mjpeg_delivers(&h, "/preview.mjpeg", h.preview_tx.clone(), b"JPGPROG").await;
    }

    #[tokio::test]
    async fn mjpeg_standby_via_route_and_deck_query() {
        let h = harness("mjpeg-b");
        // Route dédiée…
        assert_mjpeg_delivers(&h, "/preview-b.mjpeg", h.preview_b_tx.clone(), b"JPGSTBY").await;
        // …et alias de compat INTERFACES.md : ?deck=preview.
        assert_mjpeg_delivers(
            &h,
            "/preview.mjpeg?deck=preview",
            h.preview_b_tx.clone(),
            b"JPGSTBY",
        )
        .await;
    }

    // ------------------------------------------------------------ shutdown

    #[test]
    fn shutdown_frees_the_port() {
        let h = harness("shutdown");
        let addr = h.handle.local_addr();
        drop(h); // Drop => arrêt + join du thread
        // Le port doit se re-lier après un vrai close.
        let rebound = std::net::TcpListener::bind(addr);
        assert!(rebound.is_ok(), "le port doit être libéré après shutdown");
    }
}
