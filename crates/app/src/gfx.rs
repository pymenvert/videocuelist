//! Fenêtres de sortie GL : winit 0.30 + glutin 0.32 + glow.
//!
//! Un SEUL contexte GL, rendu tour à tour sur la surface de chaque fenêtre
//! (`make_current` par fenêtre) : textures, FBO et VAO du compositor sont
//! ainsi partagés sans les pièges du partage inter-contextes. Vsync sur la
//! première fenêtre uniquement (les autres swappent sans attendre) pour ne
//! pas sérialiser les sorties.
//!
//! Robustesse spectacle :
//! - contexte demandé avec `KHR_robustness` (LoseContextOnReset) quand le
//!   driver l'accepte : `glGetGraphicsResetStatus` est interrogé chaque
//!   frame — un TDR Windows (reset GPU) est détecté et remonté à l'app
//!   (`take_fatal`) qui journalise, sauvegarde et sort avec le code 11
//!   (relance par watchdog < 5 s) ;
//! - sans robustness : des échecs GL fatals RÉPÉTÉS (make_current/swap en
//!   échec continu ≥ 2 s) déclenchent la même sortie ;
//! - les `warn!` par-frame (make_current, swap_buffers) sont THROTTLÉS à
//!   1/s avec compteur de lignes supprimées — plus jamais ~200 k lignes/h
//!   qui noient le journal au pire moment ;
//! - déconnexion/reconnexion de moniteur : la topologie est surveillée
//!   (`poll_monitors`), chaque fenêtre plein écran mémorise son moniteur
//!   par NOM + POSITION (pas seulement l'index) ; moniteur perdu ⇒ repli
//!   fenêtré + warning, moniteur retrouvé ⇒ plein écran ré-appliqué —
//!   jamais de fenêtre perdue sans trace.
//!
//! Échec d'init GL (RDP, pas de GPU) : bascule headless — l'app continue
//! (UI, OSC, cues), les sorties restent noires.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use conduite_compositor::{Compositor, OutputView, SliceDraw};
use conduite_core::{OutputCfg, OutputId};
use glutin::config::{Config, ConfigTemplateBuilder};
use glutin::context::{
    ContextAttributesBuilder, NotCurrentGlContext as _, PossiblyCurrentContext,
    PossiblyCurrentGlContext as _, Robustness,
};
use glutin::display::{Display, GetGlDisplay as _, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow as _};
use raw_window_handle::HasWindowHandle as _;
use tracing::{error, info, warn};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowId};

/// Signature `glGetGraphicsResetStatus` (KHR/ARB robustness, GL 4.5 core).
type ResetStatusFn = unsafe extern "system" fn() -> u32;

/// Échecs GL consécutifs (frames) avant sortie fatale sans robustness.
const GL_FAIL_STREAK_FATAL: u32 = 120;
/// Durée minimale d'échec continu avant sortie fatale.
const GL_FAIL_DURATION_FATAL: Duration = Duration::from_secs(2);
/// Période du throttle des warns par-frame.
const WARN_PERIOD: Duration = Duration::from_secs(1);

/// Throttle de warn par-frame : au plus un log par seconde, avec compteur
/// des occurrences supprimées depuis le dernier log.
#[derive(Debug, Default)]
struct WarnThrottle {
    last: Option<Instant>,
    suppressed: u64,
}

impl WarnThrottle {
    /// `Some(supprimées)` s'il faut logger maintenant, `None` sinon.
    fn should_log(&mut self) -> Option<u64> {
        let now = Instant::now();
        match self.last {
            Some(t) if now.duration_since(t) < WARN_PERIOD => {
                self.suppressed = self.suppressed.saturating_add(1);
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

/// Identité d'un moniteur : nom + position dans le bureau virtuel —
/// stable entre déconnexions, contrairement à l'index d'énumération.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MonitorSpec {
    name: Option<String>,
    pos: (i32, i32),
}

fn monitor_spec(m: &MonitorHandle) -> MonitorSpec {
    let pos = m.position();
    MonitorSpec {
        name: m.name(),
        pos: (pos.x, pos.y),
    }
}

/// Retrouve le moniteur cible dans la liste disponible : correspondance
/// exacte nom+position d'abord, puis nom seul (les positions du bureau
/// virtuel bougent quand la topologie change), puis position seule (nom
/// indisponible sur certaines plateformes).
fn find_monitor<'a>(avail: &'a [MonitorHandle], want: &MonitorSpec) -> Option<&'a MonitorHandle> {
    if let Some(m) = avail.iter().find(|m| monitor_spec(m) == *want) {
        return Some(m);
    }
    if want.name.is_some() {
        if let Some(m) = avail.iter().find(|m| m.name() == want.name) {
            return Some(m);
        }
        None
    } else {
        avail.iter().find(|m| {
            let p = m.position();
            (p.x, p.y) == want.pos
        })
    }
}

/// Une fenêtre de sortie et sa surface GL.
struct OutWindow {
    output: OutputId,
    window: Window,
    surface: Surface<WindowSurface>,
    vsync: bool,
    /// La config de sortie demande le plein écran.
    want_fullscreen: bool,
    /// Moniteur mémorisé (nom+position) au moment du plein écran.
    target_monitor: Option<MonitorSpec>,
    /// Replié en fenêtré faute de moniteur (warning déjà émis).
    fallback_windowed: bool,
}

/// État GL vivant (contexte unique + compositor + fenêtres).
pub struct GlState {
    display: Display,
    config: Config,
    context: PossiblyCurrentContext,
    pub compositor: Compositor,
    windows: Vec<OutWindow>,
    render_err_logged: bool,
    /// `glGetGraphicsResetStatus` si KHR/ARB robustness est disponible.
    reset_status: Option<ResetStatusFn>,
    /// Frames consécutives où AUCUNE sortie n'a pu être rendue/présentée.
    fail_streak: u32,
    fail_since: Option<Instant>,
    warn_make_current: WarnThrottle,
    warn_swap: WarnThrottle,
}

impl GlState {
    /// Un cycle GL a réussi : oublie la série d'échecs en cours.
    fn note_gl_success(&mut self) {
        self.fail_streak = 0;
        self.fail_since = None;
    }

    /// Un cycle GL a complètement échoué : compte la série (détection de
    /// perte de contexte sans extension robustness).
    fn note_gl_failure(&mut self) {
        self.fail_streak = self.fail_streak.saturating_add(1);
        self.fail_since.get_or_insert_with(Instant::now);
    }

    /// La série d'échecs a atteint le seuil fatal.
    fn gl_failure_is_fatal(&self) -> bool {
        self.fail_streak >= GL_FAIL_STREAK_FATAL
            && self
                .fail_since
                .map(|t| t.elapsed() >= GL_FAIL_DURATION_FATAL)
                .unwrap_or(false)
    }
}

/// Le sous-système graphique. `gl: None` = headless (explicite ou après
/// échec d'init).
pub struct Gfx {
    pub gl: Option<GlState>,
    /// L'init GL a échoué : ne pas réessayer à chaque frame (flag santé).
    pub failed: bool,
    /// Perte GPU détectée (TDR / échecs fatals répétés) : l'app doit
    /// journaliser, sauvegarder et sortir avec le code 11.
    fatal: Option<String>,
}

impl Gfx {
    /// Mode headless explicite (`--headless`) ou avant l'init.
    pub fn headless() -> Gfx {
        Gfx {
            gl: None,
            failed: false,
            fatal: None,
        }
    }

    /// GL prêt à rendre (au moins une fenêtre vivante).
    pub fn ready(&self) -> bool {
        self.gl.as_ref().map(|g| !g.windows.is_empty()).unwrap_or(false)
    }

    /// Perte GPU fatale détectée (consommée une seule fois).
    pub fn take_fatal(&mut self) -> Option<String> {
        self.fatal.take()
    }

    /// Sorties actuellement repliées en fenêtré faute de moniteur
    /// (publiées dans `runtime.warnings`, action « output »).
    pub fn fallback_outputs(&self) -> Vec<OutputId> {
        self.gl
            .as_ref()
            .map(|g| {
                g.windows
                    .iter()
                    .filter(|w| w.fallback_windowed)
                    .map(|w| w.output)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// (Re)crée les fenêtres pour les sorties activées. À appeler au
    /// `resumed` et quand les sorties changent (EditOp).
    pub fn ensure_windows(&mut self, el: &ActiveEventLoop, outputs: &[OutputCfg]) {
        if self.failed {
            return;
        }
        let enabled: Vec<&OutputCfg> = outputs.iter().filter(|o| o.enabled).collect();

        if self.gl.is_none() {
            let Some(first) = enabled.first() else {
                return; // aucune sortie : rien à initialiser
            };
            match init_gl(el, first) {
                Ok(state) => self.gl = Some(state),
                Err(e) => {
                    error!(target: "app::gfx", error = %e,
                        "init GL impossible : bascule headless (les sorties restent noires)");
                    self.failed = true;
                    return;
                }
            }
        }
        let Some(gl) = self.gl.as_mut() else { return };

        // Reconstruit la liste si elle ne correspond plus aux sorties.
        let current: Vec<OutputId> = gl.windows.iter().map(|w| w.output).collect();
        let wanted: Vec<OutputId> = enabled.iter().map(|o| o.id).collect();
        if current == wanted && !gl.windows.is_empty() {
            return;
        }
        gl.windows.clear();
        for (i, out) in enabled.iter().enumerate() {
            match create_window(el, gl, out, i == 0) {
                Ok(w) => gl.windows.push(w),
                Err(e) => warn!(target: "app::gfx", output = out.id, error = %e,
                    "fenêtre de sortie impossible"),
            }
        }
        info!(target: "app::gfx", count = gl.windows.len(), "fenêtres de sortie prêtes");
    }

    /// Surveille la topologie des moniteurs (à appeler périodiquement,
    /// ~0,5 Hz) : ré-applique le plein écran sur le moniteur mémorisé
    /// (nom+position) s'il est revenu, replie en fenêtré avec warning s'il
    /// a disparu. Jamais de fenêtre perdue sans trace.
    pub fn poll_monitors(&mut self, el: &ActiveEventLoop) {
        let Some(gl) = self.gl.as_mut() else { return };
        if gl.windows.iter().all(|w| !w.want_fullscreen) {
            return;
        }
        let avail: Vec<MonitorHandle> = el.available_monitors().collect();
        for win in gl.windows.iter_mut() {
            if !win.want_fullscreen {
                continue;
            }
            let Some(target) = win.target_monitor.clone() else {
                continue; // plein écran « moniteur courant » : rien à suivre
            };
            let found = find_monitor(&avail, &target);
            if win.fallback_windowed {
                if let Some(m) = found {
                    info!(target: "app::gfx", output = win.output,
                        monitor = ?target.name,
                        "moniteur retrouvé : plein écran ré-appliqué");
                    win.window
                        .set_fullscreen(Some(Fullscreen::Borderless(Some(m.clone()))));
                    win.fallback_windowed = false;
                }
                continue;
            }
            // Plein écran actif : la fenêtre est-elle toujours sur SON
            // moniteur ? (Windows la déplace sur l'écran de l'opérateur
            // quand le projecteur décroche.)
            let current = win.window.current_monitor().map(|m| monitor_spec(&m));
            let on_target = match &current {
                Some(c) => {
                    *c == target
                        || (target.name.is_some() && c.name == target.name)
                }
                None => true, // pas d'info : ne rien casser
            };
            if on_target {
                continue;
            }
            match found {
                Some(m) => {
                    warn!(target: "app::gfx", output = win.output,
                        monitor = ?target.name,
                        "fenêtre déplacée hors de son moniteur : plein écran ré-appliqué");
                    win.window
                        .set_fullscreen(Some(Fullscreen::Borderless(Some(m.clone()))));
                }
                None => {
                    error!(target: "app::gfx", output = win.output,
                        monitor = ?target.name,
                        "moniteur de sortie PERDU : repli fenêtré — rebranchez \
                         l'écran, le plein écran sera ré-appliqué automatiquement");
                    win.window.set_fullscreen(None);
                    win.fallback_windowed = true;
                }
            }
        }
    }

    /// Sortie associée à une fenêtre winit (gestion des événements).
    pub fn output_of(&self, id: WindowId) -> Option<OutputId> {
        self.gl
            .as_ref()?
            .windows
            .iter()
            .find(|w| w.window.id() == id)
            .map(|w| w.output)
    }

    /// Redimensionnement d'une fenêtre : retaille la surface GL.
    pub fn resized(&mut self, id: WindowId, width: u32, height: u32) {
        let Some(gl) = self.gl.as_mut() else { return };
        let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            return; // minimisée
        };
        if let Some(win) = gl.windows.iter().find(|w| w.window.id() == id) {
            win.surface.resize(&gl.context, w, h);
        }
    }

    /// Rend le contexte courant sur la première fenêtre (uploads, préview).
    /// Échec throttlé (1/s) et compté pour la détection de perte GPU.
    pub fn make_root_current(&mut self) -> bool {
        let Some(gl) = self.gl.as_mut() else { return false };
        let Some(first) = gl.windows.first() else { return false };
        match gl.context.make_current(&first.surface) {
            Ok(()) => true,
            Err(e) => {
                if let Some(suppressed) = gl.warn_make_current.should_log() {
                    warn!(target: "app::gfx", error = %e, suppressed,
                        "make_current racine impossible");
                }
                gl.note_gl_failure();
                self.check_fatal_failure();
                false
            }
        }
    }

    /// Rend toutes les sorties : make_current par fenêtre, composition,
    /// swap. Les fenêtres SANS vsync sont rendues et swappées D'ABORD, la
    /// fenêtre vsync en DERNIER : son `swap_buffers` peut bloquer jusqu'au
    /// vblank (~16 ms) et ne doit jamais retarder la présentation des
    /// autres sorties. `on_presented` sert aux compteurs FPS par sortie.
    ///
    /// Interroge aussi `glGetGraphicsResetStatus` (si robustness) : un
    /// reset GPU (TDR) rend `take_fatal()` non vide.
    pub fn render_outputs(
        &mut self,
        plans: &HashMap<OutputId, Vec<SliceDraw>>,
        master: f32,
        dbo: f32,
        mut on_presented: impl FnMut(OutputId),
    ) {
        let Some(gl) = self.gl.as_mut() else { return };
        static EMPTY: &[SliceDraw] = &[];
        let order: Vec<usize> = (0..gl.windows.len())
            .filter(|i| !gl.windows[*i].vsync)
            .chain((0..gl.windows.len()).filter(|i| gl.windows[*i].vsync))
            .collect();
        let mut any_presented = false;
        for i in order {
            let win = &gl.windows[i];
            if let Err(e) = gl.context.make_current(&win.surface) {
                let output = win.output;
                if let Some(suppressed) = gl.warn_make_current.should_log() {
                    warn!(target: "app::gfx", output, error = %e, suppressed,
                        "make_current");
                }
                continue;
            }
            let size = win.window.inner_size();
            let view = OutputView {
                output_size: (size.width.max(1), size.height.max(1)),
                master,
                dbo,
                slices: plans.get(&win.output).map(|v| v.as_slice()).unwrap_or(EMPTY),
            };
            if let Err(e) = gl.compositor.render_output(&view) {
                if !gl.render_err_logged {
                    error!(target: "app::gfx", output = win.output, error = %e,
                        "rendu de sortie en échec (loggué une seule fois)");
                }
                gl.render_err_logged = true;
                continue;
            }
            if let Err(e) = win.surface.swap_buffers(&gl.context) {
                let output = win.output;
                if let Some(suppressed) = gl.warn_swap.should_log() {
                    warn!(target: "app::gfx", output, error = %e, suppressed,
                        "swap_buffers");
                }
                continue;
            }
            any_presented = true;
            on_presented(win.output);
        }

        // Santé GPU : reset explicite (robustness) ou échec continu.
        if let Some(reset_fn) = gl.reset_status {
            // Contexte courant garanti : on sort de la boucle de rendu.
            let status = unsafe { reset_fn() };
            if status != 0 && self.fatal.is_none() {
                self.fatal = Some(format!(
                    "reset GPU détecté (glGetGraphicsResetStatus = {})",
                    reset_status_name(status)
                ));
                return;
            }
        }
        if any_presented {
            gl.note_gl_success();
        } else if !gl.windows.is_empty() {
            gl.note_gl_failure();
        }
        self.check_fatal_failure();
    }

    /// Échec GL continu au-delà du seuil ⇒ fatal (équivalent TDR sans
    /// extension robustness).
    fn check_fatal_failure(&mut self) {
        let Some(gl) = self.gl.as_ref() else { return };
        if self.fatal.is_none() && gl.gl_failure_is_fatal() {
            self.fatal = Some(format!(
                "erreurs GL fatales répétées ({} frames consécutives sans présentation)",
                gl.fail_streak
            ));
        }
    }

    /// Rend la vue de préview dans le FBO dédié et lit les pixels RGBA dans
    /// `out` (lignes de bas en haut — à retourner à l'encodage JPEG) via la
    /// lecture asynchrone double-PBO du compositor : aucun stall du pipeline,
    /// la frame livrée est celle du tick préview PRÉCÉDENT du même `channel`
    /// (un canal PAR FLUX : program / standby).
    ///
    /// `cached_materials` : composer en réutilisant les FBO matériaux déjà
    /// remplis ce tick par `render_outputs` (chemin préview normal) au lieu
    /// de re-payer chaque passe ISF à pleine résolution.
    ///
    /// Retourne `false` tant qu'aucune frame n'est disponible (premier tick,
    /// redimensionnement, échec de rendu) : l'appelant saute l'envoi.
    #[allow(clippy::too_many_arguments)]
    pub fn render_preview_into(
        &mut self,
        channel: u32,
        width: u32,
        height: u32,
        slices: &[SliceDraw],
        master: f32,
        dbo: f32,
        cached_materials: bool,
        out: &mut Vec<u8>,
    ) -> bool {
        if !self.make_root_current() {
            return false;
        }
        let Some(gl) = self.gl.as_mut() else { return false };
        if let Err(e) = gl.compositor.bind_preview(width, height) {
            warn!(target: "app::gfx", error = %e, "FBO de préview impossible");
            return false;
        }
        let view = OutputView {
            output_size: (width, height),
            master,
            dbo,
            slices,
        };
        let rendered = if cached_materials {
            gl.compositor.render_output_cached_materials(&view)
        } else {
            gl.compositor.render_output(&view)
        };
        if let Err(e) = rendered {
            warn!(target: "app::gfx", error = %e, "rendu préview en échec");
            return false;
        }
        gl.compositor.read_preview_rgba_async(channel, width, height, out)
    }
}

/// Nom lisible d'un statut `glGetGraphicsResetStatus`.
fn reset_status_name(status: u32) -> String {
    match status {
        0x8253 => "GUILTY_CONTEXT_RESET".to_string(),
        0x8254 => "INNOCENT_CONTEXT_RESET".to_string(),
        0x8255 => "UNKNOWN_CONTEXT_RESET".to_string(),
        other => format!("0x{other:X}"),
    }
}

/// Charge `glGetGraphicsResetStatus` (ou ses variantes KHR/ARB) si le
/// contexte expose l'extension robustness.
fn load_reset_status(display: &Display, glow_ctx: &glow::Context) -> Option<ResetStatusFn> {
    use glow::HasContext as _;
    let exts = glow_ctx.supported_extensions();
    let has_robustness = exts.contains("GL_KHR_robustness")
        || exts.contains("GL_ARB_robustness")
        || exts.contains("GL_EXT_robustness");
    if !has_robustness {
        info!(target: "app::gfx",
            "robustness GL indisponible : détection TDR par échecs répétés");
        return None;
    }
    for name in [
        c"glGetGraphicsResetStatus",
        c"glGetGraphicsResetStatusKHR",
        c"glGetGraphicsResetStatusARB",
        c"glGetGraphicsResetStatusEXT",
    ] {
        let ptr = display.get_proc_address(name);
        if !ptr.is_null() {
            info!(target: "app::gfx", symbol = %name.to_string_lossy(),
                "robustness GL active : détection de reset GPU par frame");
            // Signature C stable (`GLenum ()`), pointeur non nul vérifié.
            let f: ResetStatusFn = unsafe { std::mem::transmute(ptr) };
            return Some(f);
        }
    }
    warn!(target: "app::gfx",
        "extension robustness annoncée mais symbole introuvable : \
         détection TDR par échecs répétés");
    None
}

/// Initialise display + contexte + compositor avec la première fenêtre.
fn init_gl(el: &ActiveEventLoop, first: &OutputCfg) -> anyhow::Result<GlState> {
    let attrs = window_attributes(el, first);
    let template = ConfigTemplateBuilder::new();
    let builder = DisplayBuilder::new().with_window_attributes(Some(attrs));
    // Le picker de `DisplayBuilder::build` DOIT retourner une `Config` : il
    // ne peut pas échouer proprement quand l'itérateur est vide (session
    // RDP, EGL headless, driver exotique — glutin ne garantit PAS une
    // config). On capture donc la panique pour la convertir en erreur : le
    // chemin « échec d'init GL → bascule headless » promis en tête de module
    // s'applique au lieu d'un crash au lancement le soir du spectacle.
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        builder.build(el, template, |mut configs| {
            configs
                .next()
                .expect("aucune config GL compatible sur cette machine")
        })
    }));
    let (window, config) = match built {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => return Err(anyhow::anyhow!("création display/fenêtre GL : {e}")),
        Err(_) => {
            return Err(anyhow::anyhow!(
                "aucune config GL compatible (itérateur de configs vide)"
            ))
        }
    };
    let window = window.ok_or_else(|| anyhow::anyhow!("fenêtre GL absente"))?;
    let display = config.display();

    let raw = window
        .window_handle()
        .map_err(|e| anyhow::anyhow!("window handle : {e}"))?
        .as_raw();
    // Contexte robuste d'abord (KHR_robustness : reset GPU détectable),
    // repli sur un contexte standard si le driver refuse.
    let robust_attrs = ContextAttributesBuilder::new()
        .with_robustness(Robustness::RobustLoseContextOnReset)
        .build(Some(raw));
    let not_current = match unsafe { display.create_context(&config, &robust_attrs) } {
        Ok(ctx) => ctx,
        Err(e) => {
            info!(target: "app::gfx", error = %e,
                "contexte robuste refusé par le driver : contexte standard");
            let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw));
            unsafe { display.create_context(&config, &ctx_attrs) }
                .map_err(|e| anyhow::anyhow!("création du contexte GL : {e}"))?
        }
    };

    let surf_attrs = window
        .build_surface_attributes(Default::default())
        .map_err(|e| anyhow::anyhow!("attributs de surface : {e}"))?;
    let surface = unsafe { display.create_window_surface(&config, &surf_attrs) }
        .map_err(|e| anyhow::anyhow!("création de surface : {e}"))?;
    let context = not_current
        .make_current(&surface)
        .map_err(|e| anyhow::anyhow!("make_current initial : {e}"))?;

    // Vsync sur la première fenêtre.
    if let Some(one) = NonZeroU32::new(1) {
        if let Err(e) = surface.set_swap_interval(&context, SwapInterval::Wait(one)) {
            warn!(target: "app::gfx", error = %e, "vsync indisponible");
        }
    }

    let glow_ctx = unsafe {
        glow::Context::from_loader_function_cstr(|s| display.get_proc_address(s).cast())
    };
    let reset_status = load_reset_status(&display, &glow_ctx);
    let compositor = Compositor::new(Arc::new(glow_ctx))
        .map_err(|e| anyhow::anyhow!("init compositor : {e}"))?;

    // Moniteur cible mémorisé (nom+position) pour la reconnexion.
    let target_monitor = if first.fullscreen {
        window.current_monitor().map(|m| monitor_spec(&m))
    } else {
        None
    };

    Ok(GlState {
        display,
        config,
        context,
        compositor,
        windows: vec![OutWindow {
            output: first.id,
            window,
            surface,
            vsync: true,
            want_fullscreen: first.fullscreen,
            target_monitor,
            fallback_windowed: false,
        }],
        render_err_logged: false,
        reset_status,
        fail_streak: 0,
        fail_since: None,
        warn_make_current: WarnThrottle::default(),
        warn_swap: WarnThrottle::default(),
    })
}

/// Crée une fenêtre de sortie supplémentaire sur le display existant.
fn create_window(
    el: &ActiveEventLoop,
    gl: &GlState,
    out: &OutputCfg,
    vsync: bool,
) -> anyhow::Result<OutWindow> {
    let window = el
        .create_window(window_attributes(el, out))
        .map_err(|e| anyhow::anyhow!("création de fenêtre : {e}"))?;
    let surf_attrs = window
        .build_surface_attributes(Default::default())
        .map_err(|e| anyhow::anyhow!("attributs de surface : {e}"))?;
    let surface = unsafe { gl.display.create_window_surface(&gl.config, &surf_attrs) }
        .map_err(|e| anyhow::anyhow!("création de surface : {e}"))?;
    // Intervalle de swap : vsync sur la première, immédiat ailleurs.
    if gl.context.make_current(&surface).is_ok() {
        let interval = if vsync {
            NonZeroU32::new(1).map(SwapInterval::Wait).unwrap_or(SwapInterval::DontWait)
        } else {
            SwapInterval::DontWait
        };
        if let Err(e) = surface.set_swap_interval(&gl.context, interval) {
            warn!(target: "app::gfx", output = out.id, error = %e, "swap interval");
        }
    }
    let target_monitor = if out.fullscreen {
        window.current_monitor().map(|m| monitor_spec(&m))
    } else {
        None
    };
    Ok(OutWindow {
        output: out.id,
        window,
        surface,
        vsync,
        want_fullscreen: out.fullscreen,
        target_monitor,
        fallback_windowed: false,
    })
}

/// Attributs winit d'une fenêtre de sortie : fenêtrée décorée par défaut,
/// borderless plein écran sur le moniteur demandé si `fullscreen`.
fn window_attributes(el: &ActiveEventLoop, out: &OutputCfg) -> winit::window::WindowAttributes {
    let mut attrs = Window::default_attributes()
        .with_title(format!("Conduite — {}", out.name))
        .with_inner_size(winit::dpi::PhysicalSize::new(out.width.max(64), out.height.max(64)));
    if out.fullscreen {
        let monitor = out
            .monitor_index
            .and_then(|i| el.available_monitors().nth(i));
        if out.monitor_index.is_some() && monitor.is_none() {
            warn!(target: "app::gfx", output = out.id, index = ?out.monitor_index,
                "moniteur demandé introuvable : plein écran sur le moniteur courant");
        }
        attrs = attrs
            .with_decorations(false)
            .with_fullscreen(Some(Fullscreen::Borderless(monitor)));
    }
    attrs
}
