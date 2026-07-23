//! Démo locale : sert la webui avec le show de démonstration de `core`,
//! un runtime animé et un faux journal. Usage :
//! `cargo run -p conduite-control-http --example demo` puis http://127.0.0.1:8787
//!
//! Les commandes reçues de l'UI sont affichées sur la sortie tracing.

use std::time::Duration;

use conduite_core::{demo_show, RuntimeStatus};
use conduite_control_http::{HttpDeps, HttpServer};
use serde_json::json;

fn main() {
    tracing_subscriber_init();

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let show = demo_show();
    let mut runtime = RuntimeStatus {
        active: show.cues.first().map(|c| c.number),
        standby: show.cues.get(1).map(|c| c.number),
        ..RuntimeStatus::default()
    };

    let state = json!({ "show": show, "runtime": runtime });
    let (state_tx, state_rx) = tokio::sync::watch::channel(state);
    let (events_tx, events_rx) = tokio::sync::broadcast::channel(64);
    let (_preview_tx, preview_rx) = tokio::sync::broadcast::channel(8);
    let (_preview_b_tx, preview_b_rx) = tokio::sync::broadcast::channel(8);

    let deps = HttpDeps {
        cmd_tx,
        state_rx,
        events_rx,
        preview_rx,
        preview_b_rx,
        thumb_dir: std::env::temp_dir(),
    };
    let handle = HttpServer::spawn("127.0.0.1:8787".parse().expect("addr"), deps).expect("spawn");
    println!("Démo : http://{}", handle.local_addr());

    // Écho des commandes reçues.
    std::thread::spawn(move || {
        while let Ok((src, cmd)) = cmd_rx.recv() {
            println!("commande reçue de {src:?} : {cmd:?}");
        }
    });

    // Runtime animé (progress qui boucle) + santé périodique.
    let mut t = 0.0f32;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        t += 0.008;
        if t > 1.0 {
            t = 0.0;
        }
        runtime.progress = t;
        runtime.remaining_s = (1.0 - t) * 12.0;
        runtime.mod_levels = vec![(1, (t * std::f32::consts::TAU).sin().abs())];
        let _ = state_tx.send(json!({ "show": demo_show(), "runtime": runtime }));
        if ((t * 100.0) as u32).is_multiple_of(20) {
            let _ = events_tx.send(json!({
                "type": "health_tick",
                "snapshot": { "fps": [[1, 60.0]], "drops": [[1, 0]],
                               "cpu_pct": 11.0, "mem_mb": 420.0, "temp_c": null }
            }));
            let _ = events_tx.send(json!({
                "type": "log_line", "level": "info", "target": "demo",
                "message": format!("tick t={t:.2}")
            }));
        }
    }
}

fn tracing_subscriber_init() {
    // La démo se contente de la sortie standard.
}
