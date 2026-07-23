//! Entrée audio réelle : capture cpal → ring buffer → analyse FFT (rustfft,
//! fenêtre de Hann 2048, hop 1024, mixdown mono) → [`FftFrame`] publiée dans
//! un `ArcSwap` lu par le tick — aucun verrou long côté thread de session.
//!
//! Fiabilité (doctrine spectacle) :
//! - ouverture impossible (device absent, occupé, débranché) : warn + nouvel
//!   essai toutes les 5 s — un micro USB branché en cours de show finit par
//!   fonctionner sans redémarrage ;
//! - flux en erreur en cours de route : warn + retour au cycle d'essais ;
//! - jamais de panic ; l'arrêt (changement de device, drop) est coopératif
//!   et joint le thread — pas de fuite de thread au re-spawn.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use conduite_modulation::FftFrame;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use rustfft::{num_complex::Complex32, FftPlanner};
use tracing::{debug, info, warn};

/// Taille de la fenêtre d'analyse (échantillons).
const FFT_SIZE: usize = 2048;
/// Avance entre deux analyses (recouvrement 50 %).
const FFT_HOP: usize = 1024;
/// Période de re-tentative quand le device est absent ou en erreur.
const RETRY_PERIOD: Duration = Duration::from_secs(5);
/// Granularité des attentes du worker (réactivité du stop/join).
const POLL_PERIOD: Duration = Duration::from_millis(20);
/// Capacité du ring buffer capture → FFT (≈ 1 s de mono à 48 kHz).
const RING_CAPACITY: usize = 48_000;
/// Période de rafraîchissement de la liste des devices pendant la capture.
const ENUM_PERIOD: Duration = Duration::from_secs(5);

/// Nom de device signifiant « entrée par défaut de l'OS » (config/UI).
pub const DEFAULT_DEVICE: &str = "default";

/// Verrouille sans panic : un lock empoisonné (panic d'un autre thread,
/// déjà journalisé par le hook) rend quand même la donnée.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// État partagé entre le worker audio et le thread de session.
struct Shared {
    /// Dernière trame d'analyse (vide tant qu'aucune capture n'est active).
    frame: ArcSwap<FftFrame>,
    /// Device réellement ouvert (None = pas de capture en cours).
    active: Mutex<Option<String>>,
    /// Devices d'entrée énumérés au dernier cycle (onglet Réglages).
    devices: Mutex<Vec<String>>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            frame: ArcSwap::from_pointee(FftFrame::empty()),
            active: Mutex::new(None),
            devices: Mutex::new(Vec::new()),
        }
    }
}

/// L'entrée audio de la session : possède le thread de capture/analyse et
/// expose la dernière trame FFT sans verrou long.
pub struct AudioInput {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Device demandé (`None` = capture coupée) — détection des changements.
    requested: Option<String>,
}

impl AudioInput {
    /// Construit l'entrée et démarre la capture si un device est demandé.
    pub fn new(requested: Option<String>) -> AudioInput {
        let mut input = AudioInput {
            shared: Arc::new(Shared::new()),
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            requested: None,
        };
        // Liste des devices disponible dès le boot, même capture coupée
        // (l'onglet Réglages doit pouvoir proposer un choix).
        refresh_devices(&input.shared);
        if requested.is_none() {
            info!(target: "app::audio",
                "entrée audio non configurée : FFT inactive (réglable à \
                 chaud dans Réglages, ou audio_input dans config.toml)");
        }
        input.set_device(requested);
        input
    }

    /// Change le device demandé À CHAUD : no-op si identique, sinon arrêt
    /// propre (join) du worker courant puis re-spawn. `None` coupe la capture.
    pub fn set_device(&mut self, requested: Option<String>) {
        let respawn_needed = self.requested != requested
            || (requested.is_some() && self.worker.is_none());
        if !respawn_needed {
            return;
        }
        self.stop_worker();
        self.requested = requested.clone();
        let Some(name) = requested else {
            info!(target: "app::audio", "entrée audio désactivée");
            return;
        };
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&self.stop);
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("conduite-audio".into())
            .spawn(move || worker_loop(&name, &shared, &stop));
        match spawned {
            Ok(h) => self.worker = Some(h),
            Err(e) => warn!(target: "app::audio", error = %e,
                "thread audio impossible : entrée audio inactive"),
        }
    }

    /// Arrête et joint le worker (attente courte : le worker vérifie le flag
    /// toutes les [`POLL_PERIOD`]) puis republie une trame vide.
    fn stop_worker(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.worker.take() {
            if h.join().is_err() {
                warn!(target: "app::audio", "le thread audio s'est terminé en panique");
            }
        }
        self.shared.frame.store(Arc::new(FftFrame::empty()));
        *lock(&self.shared.active) = None;
    }

    /// Trame FFT la plus récente (vide sans capture) — lecture lock-free.
    pub fn latest(&self) -> Arc<FftFrame> {
        self.shared.frame.load_full()
    }

    /// Device réellement ouvert (None = capture coupée ou en re-tentative).
    pub fn active_device(&self) -> Option<String> {
        lock(&self.shared.active).clone()
    }

    /// Devices d'entrée énumérés au dernier cycle du worker.
    pub fn devices(&self) -> Vec<String> {
        lock(&self.shared.devices).clone()
    }
}

impl Drop for AudioInput {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

// ------------------------------------------------------------------- worker

/// Boucle du thread `conduite-audio` : énumération, ouverture (avec
/// re-tentative toutes les 5 s), analyse jusqu'à erreur de flux ou stop.
fn worker_loop(requested: &str, shared: &Shared, stop: &AtomicBool) {
    info!(target: "app::audio", device = requested, "thread audio démarré");
    let mut last_err: Option<String> = None;
    while !stop.load(Ordering::SeqCst) {
        refresh_devices(shared);
        match open_capture(requested) {
            Ok(capture) => {
                last_err = None;
                run_capture(capture, shared, stop);
                shared.frame.store(Arc::new(FftFrame::empty()));
                *lock(&shared.active) = None;
                if !stop.load(Ordering::SeqCst) {
                    warn!(target: "app::audio", device = requested,
                        "capture audio interrompue (device débranché ?) : \
                         nouvel essai dans 5 s");
                    sleep_cancellable(RETRY_PERIOD, stop);
                }
            }
            Err(e) => {
                // Warn au premier échec et à chaque changement de cause ;
                // les répétitions identiques restent en debug (pas de spam
                // toutes les 5 s pendant tout un show sans micro).
                if last_err.as_deref() != Some(e.as_str()) {
                    warn!(target: "app::audio", device = requested, error = %e,
                        "ouverture de l'entrée audio impossible : nouvel \
                         essai toutes les 5 s");
                    last_err = Some(e);
                } else {
                    debug!(target: "app::audio", device = requested, error = %e,
                        "entrée audio toujours indisponible");
                }
                sleep_cancellable(RETRY_PERIOD, stop);
            }
        }
    }
    info!(target: "app::audio", "thread audio arrêté");
}

/// Attente annulable par le flag stop (granularité [`POLL_PERIOD`]).
fn sleep_cancellable(total: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(POLL_PERIOD);
    }
}

/// Rafraîchit la liste des devices d'entrée (worker uniquement, jamais tick).
fn refresh_devices(shared: &Shared) {
    let host = cpal::default_host();
    let mut names: Vec<String> = Vec::new();
    match host.input_devices() {
        Ok(devices) => {
            for d in devices {
                if let Ok(n) = d.name() {
                    names.push(n);
                }
            }
        }
        Err(e) => debug!(target: "app::audio", error = %e,
            "énumération des entrées audio impossible"),
    }
    *lock(&shared.devices) = names;
}

/// Flux ouvert : le stream cpal (vivant tant que la struct l'est), le ring
/// buffer côté consommateur et le drapeau d'erreur posé par cpal.
struct Capture {
    /// Tenu pour maintenir le flux en vie (drop = arrêt du flux).
    _stream: cpal::Stream,
    name: String,
    sample_rate: u32,
    cons: HeapConsumer<f32>,
    err: Arc<AtomicBool>,
}

/// Ouvre le device demandé (`"default"`/vide = device par défaut de l'OS,
/// sinon correspondance exacte sur le nom) et démarre le flux d'entrée.
fn open_capture(requested: &str) -> Result<Capture, String> {
    let host = cpal::default_host();
    let device = if requested.is_empty() || requested.eq_ignore_ascii_case(DEFAULT_DEVICE) {
        host.default_input_device()
            .ok_or_else(|| "aucun device d'entrée par défaut".to_string())?
    } else {
        host.input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().map(|n| n == requested).unwrap_or(false))
            .ok_or_else(|| format!("device d'entrée « {requested} » introuvable"))?
    };
    let name = device.name().unwrap_or_else(|_| requested.to_string());
    let config = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_rate = config.sample_rate().0;
    let channels = usize::from(config.channels()).max(1);

    let rb = HeapRb::<f32>::new(RING_CAPACITY);
    let (prod, cons) = rb.split();
    let err = Arc::new(AtomicBool::new(false));
    let stream = build_stream(&device, &config, channels, prod, Arc::clone(&err))?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Capture {
        _stream: stream,
        name,
        sample_rate,
        cons,
        err,
    })
}

/// Construit le flux d'entrée dans le format d'échantillon natif du device.
fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    channels: usize,
    prod: HeapProducer<f32>,
    err: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let stream_config: cpal::StreamConfig = config.config();
    match config.sample_format() {
        cpal::SampleFormat::F32 => typed_stream::<f32>(device, &stream_config, channels, prod, err),
        cpal::SampleFormat::I16 => typed_stream::<i16>(device, &stream_config, channels, prod, err),
        cpal::SampleFormat::U16 => typed_stream::<u16>(device, &stream_config, channels, prod, err),
        cpal::SampleFormat::I8 => typed_stream::<i8>(device, &stream_config, channels, prod, err),
        cpal::SampleFormat::I32 => typed_stream::<i32>(device, &stream_config, channels, prod, err),
        cpal::SampleFormat::U8 => typed_stream::<u8>(device, &stream_config, channels, prod, err),
        cpal::SampleFormat::U32 => typed_stream::<u32>(device, &stream_config, channels, prod, err),
        cpal::SampleFormat::F64 => typed_stream::<f64>(device, &stream_config, channels, prod, err),
        other => Err(format!("format d'échantillon non géré : {other:?}")),
    }
}

/// Flux typé : le callback cpal (thread audio de l'OS) mixe en mono et pousse
/// dans le ring buffer — jamais d'analyse ni de log dans le callback, buffer
/// plein ⇒ échantillons jetés (on analyse, on ne restitue pas).
fn typed_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut prod: HeapProducer<f32>,
    err: Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::FromSample as _;
    // Scratch mono réutilisé (croît une fois vers la taille du callback).
    let mut mono: Vec<f32> = Vec::with_capacity(4096);
    let err_cb = Arc::clone(&err);
    let data_cb = move |data: &[T], _: &cpal::InputCallbackInfo| {
        mono.clear();
        for frame in data.chunks(channels) {
            let mut acc = 0.0f32;
            for s in frame {
                acc += f32::from_sample_(*s);
            }
            mono.push(acc / channels as f32);
        }
        let _ = prod.push_slice(&mono);
    };
    let err_fn = move |e: cpal::StreamError| {
        // Log hors du chemin des données (callback d'erreur, rare).
        warn!(target: "app::audio", error = %e, "erreur du flux d'entrée audio");
        err_cb.store(true, Ordering::SeqCst);
    };
    device
        .build_input_stream(config, data_cb, err_fn, None)
        .map_err(|e| e.to_string())
}

/// Boucle d'analyse : consomme le ring buffer, fenêtre de Hann 2048 avec hop
/// 1024, publie chaque trame dans l'`ArcSwap`. Sort sur stop ou erreur de
/// flux. Normalisation : sinusoïde pleine échelle ≈ 1.0 (contrat
/// `spectrum_bins`/`band_level`).
fn run_capture(mut capture: Capture, shared: &Shared, stop: &AtomicBool) {
    info!(target: "app::audio", device = %capture.name,
        sample_rate = capture.sample_rate, "capture audio démarrée");
    *lock(&shared.active) = Some(capture.name.clone());

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let window = hann_window(FFT_SIZE);
    // Gain de cohérence : 2 / Σw — une sinusoïde d'amplitude 1 donne un pic ≈ 1.
    let window_gain = 2.0 / window.iter().sum::<f32>().max(f32::EPSILON);
    let bins_hz = capture.sample_rate as f32 / FFT_SIZE as f32;

    let mut samples: Vec<f32> = Vec::with_capacity(FFT_SIZE * 4);
    let mut chunk = vec![0.0f32; FFT_HOP];
    let mut spectrum = vec![Complex32::default(); FFT_SIZE];
    let mut scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
    let mut last_enum = Instant::now();

    while !stop.load(Ordering::SeqCst) && !capture.err.load(Ordering::SeqCst) {
        let n = capture.cons.pop_slice(&mut chunk);
        if n == 0 {
            std::thread::sleep(POLL_PERIOD);
            if last_enum.elapsed() >= ENUM_PERIOD {
                last_enum = Instant::now();
                refresh_devices(shared);
            }
            continue;
        }
        samples.extend_from_slice(&chunk[..n]);
        while samples.len() >= FFT_SIZE {
            for (i, c) in spectrum.iter_mut().enumerate() {
                *c = Complex32::new(samples[i] * window[i], 0.0);
            }
            fft.process_with_scratch(&mut spectrum, &mut scratch);
            let magnitudes: Vec<f32> = spectrum[..FFT_SIZE / 2]
                .iter()
                .map(|c| c.norm() * window_gain)
                .collect();
            shared.frame.store(Arc::new(FftFrame { bins_hz, magnitudes }));
            samples.drain(..FFT_HOP);
        }
        // Garde-fou : si l'analyse prend du retard, on jette le plus ancien
        // (analyse temps réel — l'arriéré n'a aucune valeur).
        if samples.len() > FFT_SIZE * 4 {
            let excess = samples.len() - FFT_SIZE;
            samples.drain(..excess);
        }
    }
    info!(target: "app::audio", device = %capture.name, "capture audio arrêtée");
}

/// Fenêtre de Hann périodique (taille `n`).
fn hann_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (std::f32::consts::TAU * i as f32 / n as f32).cos()))
        .collect()
}

// -------------------------------------------------------------- affichage

/// Lissage d'affichage des bandes du spectre : attaque rapide (le transitoire
/// se voit), release lent (la barre redescend en douceur). Pur, testable —
/// appelé par la session à la cadence de la trame d'état (10 Hz).
pub struct SpectrumSmoother {
    levels: Vec<f32>,
    /// Constante de temps de montée (s).
    attack_s: f32,
    /// Constante de temps de descente (s).
    release_s: f32,
}

impl SpectrumSmoother {
    pub fn new(n: usize) -> SpectrumSmoother {
        SpectrumSmoother {
            levels: vec![0.0; n],
            attack_s: 0.040,
            release_s: 0.350,
        }
    }

    /// Avance le lissage de `dt_s` vers `target` et rend les niveaux lissés.
    /// Une cible de taille différente réinitialise (changement de config).
    pub fn apply(&mut self, target: &[f32], dt_s: f32) -> &[f32] {
        if self.levels.len() != target.len() {
            self.levels = vec![0.0; target.len()];
        }
        let dt = dt_s.max(0.0);
        let up = 1.0 - (-dt / self.attack_s.max(1e-3)).exp();
        let down = 1.0 - (-dt / self.release_s.max(1e-3)).exp();
        for (level, t) in self.levels.iter_mut().zip(target) {
            let k = if *t > *level { up } else { down };
            *level += (*t - *level) * k;
            if !level.is_finite() {
                *level = 0.0;
            }
        }
        &self.levels
    }

    /// Remet toutes les barres à zéro (capture coupée).
    pub fn reset(&mut self) {
        self.levels.iter_mut().for_each(|v| *v = 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hann_window_is_zero_at_edges_and_one_at_center() {
        let w = hann_window(FFT_SIZE);
        assert_eq!(w.len(), FFT_SIZE);
        assert!(w[0].abs() < 1e-6);
        assert!((w[FFT_SIZE / 2] - 1.0).abs() < 1e-6);
        assert!(w.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    /// Sinusoïde pleine échelle : le pic normalisé vaut ≈ 1.0 au bin attendu
    /// (contrat d'entrée de `spectrum_bins` et `band_level`).
    #[test]
    fn full_scale_sine_normalizes_to_one() {
        let sample_rate = 48_000.0f32;
        let bin = 100; // 100 × 23.4 Hz ≈ 2.34 kHz, pile sur un bin
        let freq = bin as f32 * sample_rate / FFT_SIZE as f32;
        let window = hann_window(FFT_SIZE);
        let gain = 2.0 / window.iter().sum::<f32>();
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let mut buf: Vec<Complex32> = (0..FFT_SIZE)
            .map(|i| {
                let s = (std::f32::consts::TAU * freq * i as f32 / sample_rate).sin();
                Complex32::new(s * window[i], 0.0)
            })
            .collect();
        fft.process(&mut buf);
        let mag = buf[bin].norm() * gain;
        assert!((mag - 1.0).abs() < 0.01, "pic attendu ≈ 1.0, obtenu {mag}");
    }

    #[test]
    fn smoother_attacks_fast_and_releases_slow() {
        let mut s = SpectrumSmoother::new(1);
        let up = s.apply(&[1.0], 0.1)[0];
        assert!(up > 0.85, "attaque rapide attendue, obtenu {up}");
        let down = s.apply(&[0.0], 0.1)[0];
        assert!(down > 0.5 * up, "release lent attendu, obtenu {down} depuis {up}");
        assert!(down < up, "la barre doit quand même descendre");
    }

    #[test]
    fn smoother_converges_and_resets() {
        let mut s = SpectrumSmoother::new(2);
        for _ in 0..100 {
            s.apply(&[0.8, 0.2], 0.1);
        }
        let levels = s.apply(&[0.8, 0.2], 0.1).to_vec();
        assert!((levels[0] - 0.8).abs() < 0.01);
        assert!((levels[1] - 0.2).abs() < 0.01);
        s.reset();
        assert_eq!(s.apply(&[0.0, 0.0], 0.0), &[0.0, 0.0]);
    }

    #[test]
    fn smoother_adapts_to_target_size_change() {
        let mut s = SpectrumSmoother::new(4);
        s.apply(&[1.0; 4], 0.1);
        let out = s.apply(&[0.5; 64], 0.1);
        assert_eq!(out.len(), 64, "re-dimensionné sans panic");
    }

    #[test]
    fn smoother_swallows_non_finite_input() {
        let mut s = SpectrumSmoother::new(2);
        let out = s.apply(&[f32::NAN, f32::INFINITY], 0.1);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    /// Le cycle spawn/stop ne fuit pas de thread et ne panique pas, même
    /// sans aucun device audio sur la machine (CI).
    #[test]
    fn set_device_respawns_and_stops_cleanly() {
        let mut a = AudioInput::new(None);
        assert!(a.latest().magnitudes.is_empty());
        assert_eq!(a.active_device(), None);
        a.set_device(Some("default".to_string()));
        a.set_device(Some("default".to_string())); // no-op (inchangé)
        a.set_device(Some("un-device-qui-n-existe-pas".to_string()));
        a.set_device(None); // coupe et joint
        assert!(a.latest().magnitudes.is_empty());
        assert_eq!(a.active_device(), None);
    }
}
