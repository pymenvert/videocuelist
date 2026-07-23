//! Fenêtres de sortie GL : winit 0.30 + glutin 0.32 + glow.
//!
//! Un SEUL contexte GL, rendu tour à tour sur la surface de chaque fenêtre
//! (`make_current` par fenêtre) : textures, FBO et VAO du compositor sont
//! ainsi partagés sans les pièges du partage inter-contextes. Vsync sur la
//! première fenêtre uniquement (les autres swappent sans attendre) pour ne
//! pas sérialiser les sorties.
//!
//! Échec d'init GL (RDP, pas de GPU) : bascule headless — l'app continue
//! (UI, OSC, cues), les sorties restent noires.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use conduite_compositor::{Compositor, OutputView, SliceDraw};
use conduite_core::{OutputCfg, OutputId};
use glutin::config::{Config, ConfigTemplateBuilder};
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext as _, PossiblyCurrentContext, PossiblyCurrentGlContext as _};
use glutin::display::{Display, GetGlDisplay as _, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow as _};
use raw_window_handle::HasWindowHandle as _;
use tracing::{error, info, warn};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Fullscreen, Window, WindowId};

/// Une fenêtre de sortie et sa surface GL.
struct OutWindow {
    output: OutputId,
    window: Window,
    surface: Surface<WindowSurface>,
    vsync: bool,
}

/// État GL vivant (contexte unique + compositor + fenêtres).
pub struct GlState {
    display: Display,
    config: Config,
    context: PossiblyCurrentContext,
    pub compositor: Compositor,
    windows: Vec<OutWindow>,
    render_err_logged: bool,
}

/// Le sous-système graphique. `gl: None` = headless (explicite ou après
/// échec d'init).
pub struct Gfx {
    pub gl: Option<GlState>,
    /// L'init GL a échoué : ne pas réessayer à chaque frame (flag santé).
    pub failed: bool,
}

impl Gfx {
    /// Mode headless explicite (`--headless`) ou avant l'init.
    pub fn headless() -> Gfx {
        Gfx { gl: None, failed: false }
    }

    /// GL prêt à rendre (au moins une fenêtre vivante).
    pub fn ready(&self) -> bool {
        self.gl.as_ref().map(|g| !g.windows.is_empty()).unwrap_or(false)
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
    pub fn make_root_current(&self) -> bool {
        let Some(gl) = self.gl.as_ref() else { return false };
        let Some(first) = gl.windows.first() else { return false };
        match gl.context.make_current(&first.surface) {
            Ok(()) => true,
            Err(e) => {
                warn!(target: "app::gfx", error = %e, "make_current racine impossible");
                false
            }
        }
    }

    /// Rend toutes les sorties : make_current par fenêtre, composition,
    /// swap (vsync sur la première uniquement). `on_presented` sert aux
    /// compteurs FPS par sortie.
    pub fn render_outputs(
        &mut self,
        plans: &HashMap<OutputId, Vec<SliceDraw>>,
        master: f32,
        dbo: f32,
        mut on_presented: impl FnMut(OutputId),
    ) {
        let Some(gl) = self.gl.as_mut() else { return };
        static EMPTY: &[SliceDraw] = &[];
        for win in &gl.windows {
            if let Err(e) = gl.context.make_current(&win.surface) {
                warn!(target: "app::gfx", output = win.output, error = %e, "make_current");
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
            let _ = win.vsync; // l'intervalle de swap est posé à la création
            if let Err(e) = win.surface.swap_buffers(&gl.context) {
                warn!(target: "app::gfx", output = win.output, error = %e, "swap_buffers");
            }
            on_presented(win.output);
        }
    }

    /// Rend la vue de préview dans le FBO dédié et lit les pixels RGBA
    /// (lignes de bas en haut — à retourner à l'encodage JPEG).
    pub fn render_preview(
        &mut self,
        width: u32,
        height: u32,
        slices: &[SliceDraw],
        master: f32,
        dbo: f32,
    ) -> Option<Vec<u8>> {
        if !self.make_root_current() {
            return None;
        }
        let gl = self.gl.as_mut()?;
        if let Err(e) = gl.compositor.bind_preview(width, height) {
            warn!(target: "app::gfx", error = %e, "FBO de préview impossible");
            return None;
        }
        let view = OutputView {
            output_size: (width, height),
            master,
            dbo,
            slices,
        };
        if let Err(e) = gl.compositor.render_output(&view) {
            warn!(target: "app::gfx", error = %e, "rendu préview en échec");
            return None;
        }
        Some(gl.compositor.read_preview_rgba(width, height))
    }
}

/// Initialise display + contexte + compositor avec la première fenêtre.
fn init_gl(el: &ActiveEventLoop, first: &OutputCfg) -> anyhow::Result<GlState> {
    let attrs = window_attributes(el, first);
    let template = ConfigTemplateBuilder::new();
    let builder = DisplayBuilder::new().with_window_attributes(Some(attrs));
    let (window, config) = builder
        .build(el, template, |mut configs| {
            configs
                .next()
                .expect("glutin garantit au moins une config compatible")
        })
        .map_err(|e| anyhow::anyhow!("création display/fenêtre GL : {e}"))?;
    let window = window.ok_or_else(|| anyhow::anyhow!("fenêtre GL absente"))?;
    let display = config.display();

    let raw = window
        .window_handle()
        .map_err(|e| anyhow::anyhow!("window handle : {e}"))?
        .as_raw();
    let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw));
    let not_current = unsafe { display.create_context(&config, &ctx_attrs) }
        .map_err(|e| anyhow::anyhow!("création du contexte GL : {e}"))?;

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
    let compositor = Compositor::new(Arc::new(glow_ctx))
        .map_err(|e| anyhow::anyhow!("init compositor : {e}"))?;

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
        }],
        render_err_logged: false,
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
    Ok(OutWindow {
        output: out.id,
        window,
        surface,
        vsync,
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
