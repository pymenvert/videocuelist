//! Implémentation GL du [`Compositor`] (glow). Aucune fenêtre ici : le
//! contexte est injecté par `app` et supposé courant à chaque appel.
//!
//! Perf : textures persistantes, buffers réutilisés, uniform locations mises
//! en cache — zéro allocation dans `render_output` en régime établi.

use std::collections::HashMap;
use std::sync::Arc;

use glow::HasContext;

use conduite_core::{MaterialId, ParamValue, PatternKind, SliceId};
use conduite_engine::FrameRgba;
use conduite_isf::IsfSources;

use crate::cache::ProgramCache;
use crate::shaders::{self, composite_fragment_source, composite_vertex_source, GlslVersion};
use crate::{
    blend_code, blend_func_for, homography, merge_uniforms, sort_indices_by_z, CompositorError,
    DeckSlot, OutputView,
};

type Gl = glow::Context;
type GlProgram = <Gl as HasContext>::Program;
type GlTexture = <Gl as HasContext>::Texture;
type GlFramebuffer = <Gl as HasContext>::Framebuffer;
type GlVertexArray = <Gl as HasContext>::VertexArray;
type GlUniformLocation = <Gl as HasContext>::UniformLocation;
type GlBuffer = <Gl as HasContext>::Buffer;

/// Taille par défaut du rendu ISF quand `RENDERSIZE` n'a pas (encore) été
/// fourni par `app`.
const DEFAULT_MATERIAL_SIZE: (u32, u32) = (1280, 720);

/// Texture 2D RGBA8 persistante.
struct Tex2d {
    id: GlTexture,
    w: u32,
    h: u32,
}

/// Cible de rendu offscreen (FBO + texture couleur).
struct RenderTarget {
    fbo: GlFramebuffer,
    tex: Tex2d,
}

/// Matériau ISF actif sur un deck d'un slice.
struct MaterialSlot {
    material: MaterialId,
    /// FBO de rendu du matériau (alloué/retaillé au premier rendu).
    target: Option<RenderTarget>,
    /// Valeurs d'uniforms par nom (F/B/I/Color/P2 + TIME/RENDERSIZE/…).
    uniforms: Vec<(String, ParamValue)>,
}

/// Ring de 2 PBO `GL_PIXEL_UNPACK_BUFFER` : l'upload d'une frame passe par
/// un buffer orphané puis rempli (`glBufferData(NULL)` + `glBufferSubData`),
/// et `glTexSubImage2D` lit depuis le PBO — le transfert vers la texture
/// devient asynchrone côté driver au lieu d'une copie client synchrone.
/// Disponible en GL 3.3 core ET GLES 3.0.
struct UploadRing {
    bufs: [GlBuffer; 2],
    /// Index du PBO utilisé pour le prochain upload.
    next: usize,
}

/// Double PBO `GL_PIXEL_PACK_BUFFER` pour la lecture préview : `glReadPixels`
/// est lancé vers un PBO (retour immédiat, pas de vidage du pipeline) et on
/// relit la frame N-1 depuis l'autre PBO — latence d'une frame préview,
/// zéro stall. Un canal par flux (program / standby).
struct ReadbackChannel {
    bufs: [GlBuffer; 2],
    /// Index du PBO qui recevra la lecture de CETTE frame.
    write: usize,
    /// Une lecture a-t-elle déjà été lancée dans ce PBO ?
    primed: [bool; 2],
    w: u32,
    h: u32,
}

/// Un deck (A ou B) d'un slice : texture vidéo + matériau optionnel.
#[derive(Default)]
struct DeckRes {
    video: Option<Tex2d>,
    material: Option<MaterialSlot>,
    /// PBO d'upload — créés au premier upload, `None` si PBO indisponibles.
    upload: Option<UploadRing>,
}

/// Ressources GPU d'un slice.
#[derive(Default)]
struct SliceRes {
    decks: [DeckRes; 2],
}

/// Programme ISF compilé : locations et unités de texture pré-résolues.
struct IsfProgram {
    program: GlProgram,
    uniforms: HashMap<String, GlUniformLocation>,
    /// Samplers actifs : (nom, unité de texture), unités assignées au link.
    samplers: Vec<(String, u32)>,
}

/// Locations du programme de composition (résolues une fois au link).
struct CompositeLocs {
    homography: Option<GlUniformLocation>,
    src_rect: Option<GlUniformLocation>,
    mix: Option<GlUniformLocation>,
    brightness: Option<GlUniformLocation>,
    contrast: Option<GlUniformLocation>,
    gamma: Option<GlUniformLocation>,
    gain: Option<GlUniformLocation>,
    opacity: Option<GlUniformLocation>,
    black: Option<GlUniformLocation>,
    master: Option<GlUniformLocation>,
    pattern: Option<GlUniformLocation>,
    slice_num: Option<GlUniformLocation>,
    blend_mode: Option<GlUniformLocation>,
}

/// Le compositor : tout le GL du produit. Créé avec un contexte partagé
/// entre les fenêtres de sortie ; `app` rend le bon contexte courant avant
/// chaque appel.
pub struct Compositor {
    gl: Arc<Gl>,
    glsl: GlslVersion,
    composite: GlProgram,
    locs: CompositeLocs,
    /// VAO vide requis en core profile pour dessiner via `gl_VertexID`.
    quad_vao: GlVertexArray,
    /// Texture noire 1×1 : fallback pour tout sampler sans contenu.
    black_tex: GlTexture,
    slices: HashMap<SliceId, SliceRes>,
    isf_cache: ProgramCache<IsfProgram>,
    /// FBO de préview (MJPEG) — retaillé à la demande.
    preview: Option<RenderTarget>,
    /// Canaux de lecture préview asynchrone (double PBO), par flux.
    readbacks: HashMap<u32, ReadbackChannel>,
    /// Création de PBO en échec : replis synchrones, loggé une seule fois.
    pbo_broken: bool,
    /// Framebuffer cible courant (None = framebuffer par défaut).
    current_target: Option<GlFramebuffer>,
    /// Scratch de tri par z, réutilisé chaque frame.
    z_indices: Vec<usize>,
}

impl Compositor {
    /// Initialise le pipeline : sélection du dialecte GLSL (330 core ou
    /// 300 es selon le contexte), compilation du programme de composition,
    /// ressources partagées.
    pub fn new(gl: Arc<Gl>) -> Result<Self, CompositorError> {
        let version = gl.version();
        let glsl = if version.is_embedded {
            GlslVersion::Es300
        } else {
            GlslVersion::Core330
        };
        tracing::info!(
            target: "compositor",
            major = version.major,
            minor = version.minor,
            embedded = version.is_embedded,
            ?glsl,
            "initialisation du compositor"
        );

        let composite = link_program(
            &gl,
            &composite_vertex_source(glsl),
            &composite_fragment_source(glsl),
        )?;

        let locs = unsafe {
            CompositeLocs {
                homography: gl.get_uniform_location(composite, "u_homography"),
                src_rect: gl.get_uniform_location(composite, "u_src_rect"),
                mix: gl.get_uniform_location(composite, "u_mix"),
                brightness: gl.get_uniform_location(composite, "u_brightness"),
                contrast: gl.get_uniform_location(composite, "u_contrast"),
                gamma: gl.get_uniform_location(composite, "u_gamma"),
                gain: gl.get_uniform_location(composite, "u_gain"),
                opacity: gl.get_uniform_location(composite, "u_opacity"),
                black: gl.get_uniform_location(composite, "u_black"),
                master: gl.get_uniform_location(composite, "u_master"),
                pattern: gl.get_uniform_location(composite, "u_pattern"),
                slice_num: gl.get_uniform_location(composite, "u_slice_num"),
                blend_mode: gl.get_uniform_location(composite, "u_blend_mode"),
            }
        };

        // Les samplers de composition sont fixes : A = unité 0, B = unité 1.
        unsafe {
            gl.use_program(Some(composite));
            let a = gl.get_uniform_location(composite, "u_src_a");
            gl.uniform_1_i32(a.as_ref(), 0);
            let b = gl.get_uniform_location(composite, "u_src_b");
            gl.uniform_1_i32(b.as_ref(), 1);
            gl.use_program(None);
        }

        let quad_vao = unsafe { gl.create_vertex_array() }.map_err(CompositorError::Init)?;
        let black_tex = create_texture_rgba(&gl, 1, 1, Some(&[0, 0, 0, 255]))?;

        Ok(Self {
            gl,
            glsl,
            composite,
            locs,
            quad_vao,
            black_tex,
            slices: HashMap::new(),
            isf_cache: ProgramCache::default(),
            preview: None,
            readbacks: HashMap::new(),
            pbo_broken: false,
            current_target: None,
            z_indices: Vec::with_capacity(32),
        })
    }

    /// Dialecte GLSL sélectionné à l'init.
    pub fn glsl_version(&self) -> GlslVersion {
        self.glsl
    }

    /// Garantit les ressources d'un slice : deux textures RGBA (decks A/B),
    /// noires 1×1 au départ, réallouées quand les dimensions changent.
    pub fn ensure_slice_textures(&mut self, slice: SliceId) {
        let entry = self.slices.entry(slice).or_default();
        for deck in &mut entry.decks {
            if deck.video.is_none() {
                match create_texture_rgba(&self.gl, 1, 1, Some(&[0, 0, 0, 255])) {
                    Ok(id) => deck.video = Some(Tex2d { id, w: 1, h: 1 }),
                    Err(e) => {
                        tracing::error!(target: "compositor", slice, %e, "création de texture");
                    }
                }
            }
        }
    }

    /// Upload d'une frame décodée dans la texture du deck. La texture est
    /// réutilisée et réallouée seulement si les dimensions changent. Le
    /// chemin nominal passe par un ring de 2 PBO orphanés (transfert
    /// asynchrone côté driver, pas de copie client bloquante) ; repli en
    /// `glTexSubImage2D` direct si les PBO sont indisponibles.
    pub fn upload_frame(&mut self, slice: SliceId, deck: DeckSlot, f: &FrameRgba) {
        let expected = (f.width as usize) * (f.height as usize) * 4;
        if f.width == 0 || f.height == 0 || f.data.len() < expected {
            tracing::error!(
                target: "compositor",
                slice, w = f.width, h = f.height, len = f.data.len(),
                "frame RGBA invalide, upload ignoré"
            );
            return;
        }
        self.ensure_slice_textures(slice);
        let gl = &self.gl;
        let Some(res) = self.slices.get_mut(&slice) else {
            return;
        };
        let deck_res = &mut res.decks[deck.index()];
        let Some(tex) = deck_res.video.as_mut() else {
            return; // création ratée, déjà loggé
        };
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(tex.id));
            if tex.w != f.width || tex.h != f.height {
                // Réallocation (dimensions changées) : chemin rare, direct.
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    f.width as i32,
                    f.height as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&f.data[..expected])),
                );
                tex.w = f.width;
                tex.h = f.height;
            } else if let Some(ring) =
                ensure_upload_ring(gl, &mut deck_res.upload, &mut self.pbo_broken)
            {
                // Chemin nominal : PBO orphané puis rempli, glTexSubImage2D
                // lit depuis le buffer — le transfert devient asynchrone.
                let buf = ring.bufs[ring.next];
                ring.next ^= 1;
                gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(buf));
                // Orphaning : stockage neuf sans attendre le transfert du
                // cycle précédent (le driver recycle en interne).
                gl.buffer_data_size(
                    glow::PIXEL_UNPACK_BUFFER,
                    expected as i32,
                    glow::STREAM_DRAW,
                );
                gl.buffer_sub_data_u8_slice(glow::PIXEL_UNPACK_BUFFER, 0, &f.data[..expected]);
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    f.width as i32,
                    f.height as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::BufferOffset(0),
                );
                gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            } else {
                // Repli sans PBO : copie client synchrone (comportement
                // historique, toujours correct).
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    f.width as i32,
                    f.height as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&f.data[..expected])),
                );
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// Active (ou retire, si `isf` est `None`) un matériau ISF sur un deck.
    /// Le programme est compilé et mis en cache par clé matériau ; un échec
    /// de compilation retourne le log GLSL complet et **laisse l'état
    /// précédent intact** (jamais de panic, la texture précédente reste
    /// affichée).
    pub fn set_material(
        &mut self,
        slice: SliceId,
        deck: DeckSlot,
        isf: Option<&IsfSources>,
        material: MaterialId,
    ) -> Result<(), CompositorError> {
        self.ensure_slice_textures(slice);
        let Some(sources) = isf else {
            // Retrait du matériau : le FBO est libéré, le programme reste en
            // cache (partagé entre slices).
            if let Some(res) = self.slices.get_mut(&slice) {
                if let Some(slot) = res.decks[deck.index()].material.take() {
                    if let Some(target) = slot.target {
                        release_target(&self.gl, target);
                    }
                }
            }
            return Ok(());
        };

        let gl = &self.gl;
        // Compile + cache par clé matériau ; l'échec remonte le log complet
        // sans toucher au slot courant.
        self.isf_cache
            .ensure(material, || compile_isf_program(gl, sources))?;

        let Some(res) = self.slices.get_mut(&slice) else {
            return Ok(());
        };
        let deck_res = &mut res.decks[deck.index()];
        match deck_res.material.as_mut() {
            Some(slot) if slot.material == material => {} // déjà actif
            Some(slot) => {
                slot.material = material;
                slot.uniforms.clear();
            }
            None => {
                deck_res.material = Some(MaterialSlot {
                    material,
                    target: None,
                    uniforms: Vec::with_capacity(16),
                });
            }
        }
        tracing::debug!(target: "compositor", slice, material, "matériau actif");
        Ok(())
    }

    /// Force la recompilation d'un matériau (hot-reload). En cas d'échec le
    /// programme précédent est conservé et l'erreur (log GLSL complet)
    /// remonte à l'UI.
    pub fn reload_material(
        &mut self,
        material: MaterialId,
        sources: &IsfSources,
    ) -> Result<(), CompositorError> {
        let gl = &self.gl;
        let old = self
            .isf_cache
            .recompile(material, || compile_isf_program(gl, sources))?;
        if let Some(old) = old {
            unsafe { self.gl.delete_program(old.program) };
        }
        Ok(())
    }

    /// Préchauffe le cache de programmes ISF : compile tous les matériaux
    /// fournis qui ne sont pas déjà en cache. À appeler au chargement du show
    /// ou au passage en mode Show, pendant que rien ne joue — en spectacle,
    /// `set_material` ne fait alors que des hits de cache (jamais de
    /// `glCompileShader` sur le thread de rendu pendant un GO).
    ///
    /// Un matériau en échec n'empêche pas la compilation des suivants : les
    /// erreurs (log GLSL complet) sont collectées et retournées pour l'UI.
    pub fn prewarm(
        &mut self,
        materials: &[(MaterialId, &IsfSources)],
    ) -> Vec<(MaterialId, CompositorError)> {
        let mut errors = Vec::new();
        let mut compiled = 0u32;
        for (id, sources) in materials {
            if self.isf_cache.contains(*id) {
                continue;
            }
            let gl = &self.gl;
            match self.isf_cache.ensure(*id, || compile_isf_program(gl, sources)) {
                Ok(_) => compiled += 1,
                Err(e) => errors.push((*id, e)),
            }
        }
        tracing::info!(
            target: "compositor",
            demandes = materials.len(),
            compiles = compiled,
            echecs = errors.len(),
            "préchauffage des programmes ISF"
        );
        errors
    }

    /// Détache le matériau d'un deck sans exiger les sources ISF : le FBO du
    /// matériau est libéré, la texture vidéo redevient le contenu servi. Le
    /// programme compilé reste en cache (partagé entre slices). Contrairement
    /// à `set_material(.., None, ..)`, ne crée pas le slice s'il n'existe pas.
    pub fn detach_material(&mut self, slice: SliceId, deck: DeckSlot) {
        let Some(res) = self.slices.get_mut(&slice) else {
            return;
        };
        if let Some(slot) = res.decks[deck.index()].material.take() {
            if let Some(target) = slot.target {
                release_target(&self.gl, target);
            }
            tracing::debug!(target: "compositor", slice, "matériau détaché");
        }
    }

    /// Détache les matériaux de TOUS les decks de TOUS les slices (FBO
    /// libérés, programmes conservés en cache). À appeler à l'installation
    /// d'un show / undo / redo, AVANT de vider l'état `material_bound` côté
    /// app — sinon l'ancien shader reste affiché à la place de la vidéo
    /// (matériau fantôme) et son FBO fuit.
    pub fn detach_all_materials(&mut self) {
        for (&slice, res) in self.slices.iter_mut() {
            for deck in &mut res.decks {
                if let Some(slot) = deck.material.take() {
                    if let Some(target) = slot.target {
                        release_target(&self.gl, target);
                    }
                    tracing::debug!(target: "compositor", slice, "matériau détaché");
                }
            }
        }
    }

    /// Invalide le programme compilé d'un matériau (suppression GL incluse) :
    /// le prochain `set_material` recompilera. À appeler sur MaterialRemove.
    pub fn invalidate_material(&mut self, material: MaterialId) {
        if let Some(old) = self.isf_cache.invalidate(material) {
            unsafe { self.gl.delete_program(old.program) };
            tracing::debug!(target: "compositor", material, "programme ISF invalidé");
        }
    }

    /// Ne conserve en cache que les programmes des matériaux listés ; les
    /// programmes GL des matériaux disparus sont supprimés. À appeler à
    /// l'installation d'un show pour ne pas accumuler de programmes orphelins
    /// en VRAM pendant toute la vie du process.
    pub fn retain_materials(&mut self, keep: &[MaterialId]) {
        let evicted = self.isf_cache.retain(keep);
        let count = evicted.len();
        for old in evicted {
            unsafe { self.gl.delete_program(old.program) };
        }
        if count > 0 {
            tracing::info!(target: "compositor", evinces = count, "programmes ISF élagués");
        }
    }

    /// Élague les slices absents de `keep` : textures vidéo, PBO d'upload et
    /// FBO matériaux des slices disparus sont libérés. À appeler après
    /// SliceRemove et à l'installation d'un show — sans cet appel, chaque
    /// slice supprimé laisse ses textures pleine résolution en VRAM à vie.
    pub fn prune_slices(&mut self, keep: &[SliceId]) {
        let dead = crate::keys_not_kept(self.slices.keys().copied(), keep);
        for id in dead {
            if let Some(res) = self.slices.remove(&id) {
                release_slice(&self.gl, res);
                tracing::debug!(target: "compositor", slice = id, "slice élagué");
            }
        }
    }

    /// Pousse des valeurs d'uniforms pour le matériau d'un deck (par nom,
    /// types F/B/I/Color/P2 ; TIME/RENDERSIZE/FRAMEINDEX sont fournis par
    /// `app` par le même canal). Appliquées au prochain rendu.
    pub fn set_material_uniforms(
        &mut self,
        slice: SliceId,
        deck: DeckSlot,
        values: &[(String, ParamValue)],
    ) {
        let Some(res) = self.slices.get_mut(&slice) else {
            tracing::warn!(target: "compositor", slice, "uniforms pour un slice inconnu");
            return;
        };
        let Some(slot) = res.decks[deck.index()].material.as_mut() else {
            tracing::warn!(target: "compositor", slice, "uniforms sans matériau actif");
            return;
        };
        merge_uniforms(&mut slot.uniforms, values);
    }

    /// Rend une sortie complète dans le framebuffer courant : passes ISF
    /// offscreen, puis slices triés par z (composition warpée, crossfade A/B,
    /// blend modes, master/DBO en multiplicateur final).
    pub fn render_output(&mut self, out: &OutputView) -> Result<(), CompositorError> {
        // 1. Passes matériaux offscreen (avant de toucher au framebuffer cible).
        self.render_material_passes(out)?;
        self.render_composite(out)
    }

    /// Variante sans passes ISF : compose en réutilisant les FBO matériaux
    /// déjà remplis dans ce tick par un `render_output` précédent (mêmes
    /// uniforms pour toutes les vues). Chemin préview : évite de re-payer
    /// chaque matériau à pleine résolution (RENDERSIZE sortie) pour une cible
    /// 640×360. Ne l'appeler que si `render_output` a tourné ce tick.
    pub fn render_output_cached_materials(
        &mut self,
        out: &OutputView,
    ) -> Result<(), CompositorError> {
        self.render_composite(out)
    }

    /// Composition des slices dans le framebuffer courant (partie commune de
    /// `render_output` / `render_output_cached_materials`).
    fn render_composite(&mut self, out: &OutputView) -> Result<(), CompositorError> {
        let gl = &self.gl;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.current_target);
            gl.viewport(0, 0, out.output_size.0 as i32, out.output_size.1 as i32);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::BLEND);
            gl.bind_vertex_array(Some(self.quad_vao));
            gl.use_program(Some(self.composite));
        }

        // 2. Tri par z (scratch réutilisé — pas d'allocation par frame).
        sort_indices_by_z(&mut self.z_indices, out.slices);

        let master = (out.master.clamp(0.0, 1.0)) * (1.0 - out.dbo.clamp(0.0, 1.0));
        for idx in 0..self.z_indices.len() {
            let sd = &out.slices[self.z_indices[idx]];

            // Homographie 4 coins — coins dégénérés : slice sauté, pas de panic.
            let h = match homography::from_corners(&sd.corners) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(target: "compositor", slice = sd.slice, %e, "slice sauté");
                    continue;
                }
            };

            let (tex_a, tex_b) = self.slice_textures(sd.slice);
            let (src, dst) = blend_func_for(sd.blend_mode);
            let gl = &self.gl;
            let locs = &self.locs;
            unsafe {
                gl.blend_func(src, dst);
                gl.uniform_matrix_3_f32_slice(locs.homography.as_ref(), false, &h.to_gl());
                gl.uniform_4_f32(
                    locs.src_rect.as_ref(),
                    sd.src_rect.x,
                    sd.src_rect.y,
                    sd.src_rect.w,
                    sd.src_rect.h,
                );
                gl.uniform_1_f32(locs.mix.as_ref(), sd.mix.clamp(0.0, 1.0));
                gl.uniform_1_f32(locs.brightness.as_ref(), sd.brightness);
                gl.uniform_1_f32(locs.contrast.as_ref(), sd.contrast);
                gl.uniform_1_f32(locs.gamma.as_ref(), sd.gamma.max(0.01));
                gl.uniform_3_f32(locs.gain.as_ref(), sd.gains[0], sd.gains[1], sd.gains[2]);
                gl.uniform_1_f32(locs.opacity.as_ref(), sd.opacity.clamp(0.0, 1.0));
                gl.uniform_1_f32(locs.black.as_ref(), sd.black.clamp(0.0, 1.0));
                gl.uniform_1_f32(locs.master.as_ref(), master);
                gl.uniform_1_i32(locs.pattern.as_ref(), shaders::pattern_code(sd.pattern));
                gl.uniform_1_i32(locs.slice_num.as_ref(), sd.slice as i32);
                gl.uniform_1_i32(locs.blend_mode.as_ref(), blend_code(sd.blend_mode));

                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex_a));
                gl.active_texture(glow::TEXTURE0 + 1);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex_b));

                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
        }

        let gl = &self.gl;
        unsafe {
            gl.disable(glow::BLEND);
            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
        Ok(())
    }

    /// Rend une mire plein cadre dans le framebuffer courant (calage global,
    /// bouton « identifier » d'une sortie). `ident_num` est le numéro affiché
    /// par la mire [`PatternKind::Ident`].
    pub fn render_pattern(
        &mut self,
        output_size: (u32, u32),
        pattern: PatternKind,
        ident_num: u32,
    ) -> Result<(), CompositorError> {
        let gl = &self.gl;
        let locs = &self.locs;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.current_target);
            gl.viewport(0, 0, output_size.0 as i32, output_size.1 as i32);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::BLEND);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.use_program(Some(self.composite));
            gl.bind_vertex_array(Some(self.quad_vao));

            gl.uniform_matrix_3_f32_slice(
                locs.homography.as_ref(),
                false,
                &homography::Mat3::IDENTITY.to_gl(),
            );
            gl.uniform_4_f32(locs.src_rect.as_ref(), 0.0, 0.0, 1.0, 1.0);
            gl.uniform_1_f32(locs.mix.as_ref(), 0.0);
            gl.uniform_1_f32(locs.brightness.as_ref(), 1.0);
            gl.uniform_1_f32(locs.contrast.as_ref(), 1.0);
            gl.uniform_1_f32(locs.gamma.as_ref(), 1.0);
            gl.uniform_3_f32(locs.gain.as_ref(), 1.0, 1.0, 1.0);
            gl.uniform_1_f32(locs.opacity.as_ref(), 1.0);
            gl.uniform_1_f32(locs.black.as_ref(), 0.0);
            gl.uniform_1_f32(locs.master.as_ref(), 1.0);
            gl.uniform_1_i32(locs.pattern.as_ref(), shaders::pattern_code(Some(pattern)));
            gl.uniform_1_i32(locs.slice_num.as_ref(), ident_num as i32);
            gl.uniform_1_i32(locs.blend_mode.as_ref(), shaders::BLEND_NORMAL);

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.black_tex));
            gl.active_texture(glow::TEXTURE0 + 1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.black_tex));

            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
        Ok(())
    }

    /// Dirige les prochains `render_output`/`render_pattern` vers le FBO de
    /// préview (retaillé à `w`×`h`). Séquence MJPEG :
    /// `bind_preview` → `render_output` → [`Compositor::read_preview_rgba`].
    pub fn bind_preview(&mut self, w: u32, h: u32) -> Result<(), CompositorError> {
        let w = w.max(1);
        let h = h.max(1);
        let needs_realloc = match &self.preview {
            Some(t) => t.tex.w != w || t.tex.h != h,
            None => true,
        };
        if needs_realloc {
            if let Some(old) = self.preview.take() {
                release_target(&self.gl, old);
            }
            self.preview = Some(create_target(&self.gl, w, h)?);
        }
        // self.preview vient d'être garanti ci-dessus.
        if let Some(t) = &self.preview {
            self.current_target = Some(t.fbo);
            unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, self.current_target) };
        }
        Ok(())
    }

    /// Lit les pixels RGBA de la préview (FBO dédié, dimensions
    /// paramétrables) pour le flux MJPEG, puis rétablit le framebuffer par
    /// défaut. À appeler après `bind_preview` + `render_output`.
    ///
    /// ATTENTION : `glReadPixels` vers la mémoire client vide TOUT le
    /// pipeline (stall GPU complet). Préférer
    /// [`Compositor::read_preview_rgba_async`] sur le thread de rendu.
    pub fn read_preview_rgba(&mut self, w: u32, h: u32) -> Vec<u8> {
        let w = w.max(1);
        let h = h.max(1);
        let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        let gl = &self.gl;
        match &self.preview {
            Some(t) if t.tex.w == w && t.tex.h == h => unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
                gl.read_pixels(
                    0,
                    0,
                    w as i32,
                    h as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut pixels)),
                );
            },
            _ => {
                tracing::warn!(
                    target: "compositor",
                    w, h,
                    "read_preview_rgba sans bind_preview préalable aux mêmes dimensions"
                );
            }
        }
        // Retour au framebuffer par défaut pour les rendus suivants.
        self.current_target = None;
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
        pixels
    }

    /// Lecture asynchrone de la préview via double PBO : `glReadPixels` est
    /// lancé vers un PBO (retour immédiat, aucun vidage du pipeline) et la
    /// frame lue au tick préview PRÉCÉDENT du même `channel` est copiée dans
    /// `out` — latence d'une frame préview, invisible pour un MJPEG 8 fps,
    /// zéro stall du thread de rendu. Rétablit le framebuffer par défaut.
    ///
    /// - un `channel` PAR FLUX (program, standby) : partager un canal entre
    ///   deux flux mélangerait leurs images ;
    /// - retourne `false` tant qu'aucune frame N-1 n'est disponible (premier
    ///   tick, changement de dimensions) : l'appelant saute l'envoi ;
    /// - `out` est réutilisé entre les appels (aucune allocation en régime
    ///   établi) ;
    /// - repli synchrone transparent si les PBO sont indisponibles.
    ///
    /// À appeler après `bind_preview` + `render_output` aux mêmes dimensions.
    pub fn read_preview_rgba_async(
        &mut self,
        channel: u32,
        w: u32,
        h: u32,
        out: &mut Vec<u8>,
    ) -> bool {
        let w = w.max(1);
        let h = h.max(1);
        let fbo = match &self.preview {
            Some(t) if t.tex.w == w && t.tex.h == h => t.fbo,
            _ => {
                tracing::warn!(
                    target: "compositor",
                    w, h,
                    "read_preview_rgba_async sans bind_preview préalable aux mêmes dimensions"
                );
                self.current_target = None;
                unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
                return false;
            }
        };

        // Repli synchrone : PBO indisponibles sur ce driver.
        if self.pbo_broken {
            let pixels = self.read_preview_rgba(w, h);
            out.clear();
            out.extend_from_slice(&pixels);
            return true;
        }

        let size = (w as usize) * (h as usize) * 4;

        // (Re)création du canal si absent ou aux mauvaises dimensions.
        let stale = self
            .readbacks
            .get(&channel)
            .is_some_and(|ch| ch.w != w || ch.h != h);
        if stale {
            if let Some(ch) = self.readbacks.remove(&channel) {
                for buf in ch.bufs {
                    unsafe { self.gl.delete_buffer(buf) };
                }
            }
        }
        if !self.readbacks.contains_key(&channel) {
            match create_readback_channel(&self.gl, w, h, size) {
                Ok(ch) => {
                    self.readbacks.insert(channel, ch);
                }
                Err(e) => {
                    self.pbo_broken = true;
                    tracing::warn!(
                        target: "compositor",
                        %e,
                        "PBO indisponibles, lecture préview synchrone"
                    );
                    let pixels = self.read_preview_rgba(w, h);
                    out.clear();
                    out.extend_from_slice(&pixels);
                    return true;
                }
            }
        }

        let gl = &self.gl;
        let Some(ch) = self.readbacks.get_mut(&channel) else {
            return false; // inatteignable : inséré juste au-dessus
        };
        let mut got = false;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));

            // 1. Lecture de CETTE frame vers le PBO d'écriture : non bloquant,
            //    le transfert se fait en tâche de fond côté driver.
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(ch.bufs[ch.write]));
            gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::BufferOffset(0),
            );
            ch.primed[ch.write] = true;

            // 2. Relecture de la frame N-1 depuis l'autre PBO : le transfert
            //    a eu un tick préview complet pour se terminer, le map ne
            //    bloque pas.
            let read = ch.write ^ 1;
            if ch.primed[read] {
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(ch.bufs[read]));
                let ptr =
                    gl.map_buffer_range(glow::PIXEL_PACK_BUFFER, 0, size as i32, glow::MAP_READ_BIT);
                if !ptr.is_null() {
                    out.clear();
                    out.extend_from_slice(std::slice::from_raw_parts(ptr, size));
                    got = true;
                }
                gl.unmap_buffer(glow::PIXEL_PACK_BUFFER);
            }
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            ch.write = read;
        }

        // Retour au framebuffer par défaut pour les rendus suivants.
        self.current_target = None;
        unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
        got
    }

    // ------------------------------------------------------------- interne

    /// Textures effectives (A, B) d'un slice pour la composition : résultat
    /// ISF si un matériau est actif, sinon la texture vidéo, sinon du noir.
    fn slice_textures(&self, slice: SliceId) -> (GlTexture, GlTexture) {
        let pick = |deck: &DeckRes| -> GlTexture {
            if let Some(slot) = &deck.material {
                if let Some(t) = &slot.target {
                    return t.tex.id;
                }
            }
            deck.video
                .as_ref()
                .map(|t| t.id)
                .unwrap_or(self.black_tex)
        };
        match self.slices.get(&slice) {
            Some(res) => (pick(&res.decks[0]), pick(&res.decks[1])),
            None => (self.black_tex, self.black_tex),
        }
    }

    /// Rend chaque matériau ISF actif dans son FBO offscreen. Les decks
    /// inutiles sont sautés (mix = 0 ⇒ pas de rendu du deck B, etc.).
    fn render_material_passes(&mut self, out: &OutputView) -> Result<(), CompositorError> {
        for sd in out.slices {
            if sd.pattern.is_some() {
                continue; // mire : pas de contenu à produire
            }
            let mix = sd.mix.clamp(0.0, 1.0);
            for deck_idx in 0..2 {
                // Deck A inutile si mix = 1, deck B inutile si mix = 0.
                if (deck_idx == 0 && mix >= 1.0) || (deck_idx == 1 && mix <= 0.0) {
                    continue;
                }
                self.render_one_material(sd.slice, deck_idx)?;
            }
        }
        Ok(())
    }

    /// Passe ISF d'un deck : FBO retaillé sur RENDERSIZE, uniforms poussés,
    /// texture vidéo branchée sur `inputImage`, quad plein écran.
    fn render_one_material(
        &mut self,
        slice: SliceId,
        deck_idx: usize,
    ) -> Result<(), CompositorError> {
        // Emprunts disjoints : gl / slices / cache.
        let gl = &self.gl;
        let Some(res) = self.slices.get_mut(&slice) else {
            return Ok(());
        };
        let deck = &mut res.decks[deck_idx];
        let video_tex = deck.video.as_ref().map(|t| t.id);
        let Some(slot) = deck.material.as_mut() else {
            return Ok(());
        };
        let Some(program) = self.isf_cache.get(slot.material) else {
            // set_material a échoué ou jamais été appelé : contenu vidéo servi tel quel.
            return Ok(());
        };

        // Taille de rendu : RENDERSIZE fourni par app, sinon défaut.
        let (w, h) = slot
            .uniforms
            .iter()
            .find(|(n, _)| n == "RENDERSIZE")
            .and_then(|(_, v)| match v {
                ParamValue::P2([w, h]) => Some((*w as u32, *h as u32)),
                _ => None,
            })
            .filter(|&(w, h)| w > 0 && h > 0)
            .unwrap_or(DEFAULT_MATERIAL_SIZE);

        let needs_realloc = match &slot.target {
            Some(t) => t.tex.w != w || t.tex.h != h,
            None => true,
        };
        if needs_realloc {
            if let Some(old) = slot.target.take() {
                release_target(gl, old);
            }
            slot.target = Some(create_target(gl, w, h)?);
        }
        let Some(target) = &slot.target else {
            return Ok(());
        };

        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.fbo));
            gl.viewport(0, 0, w as i32, h as i32);
            gl.disable(glow::BLEND);
            gl.use_program(Some(program.program));
            gl.bind_vertex_array(Some(self.quad_vao));

            // Uniforms par nom (les noms inconnus du programme sont ignorés).
            for (name, value) in &slot.uniforms {
                let Some(loc) = program.uniforms.get(name) else {
                    continue;
                };
                match value {
                    ParamValue::F(v) => gl.uniform_1_f32(Some(loc), *v),
                    ParamValue::I(v) => gl.uniform_1_i32(Some(loc), *v as i32),
                    ParamValue::B(v) => gl.uniform_1_i32(Some(loc), i32::from(*v)),
                    ParamValue::Color(c) => gl.uniform_4_f32(Some(loc), c[0], c[1], c[2], c[3]),
                    ParamValue::P2(p) => gl.uniform_2_f32(Some(loc), p[0], p[1]),
                    ParamValue::S(_) => {} // pas d'uniform string en GLSL
                }
            }

            // Samplers : `inputImage` reçoit la vidéo du deck, le reste
            // (audio, audioFFT…) est nourri au noir — jamais de sampler
            // non alimenté.
            for (name, unit) in &program.samplers {
                gl.active_texture(glow::TEXTURE0 + unit);
                let tex = if name == "inputImage" {
                    video_tex.unwrap_or(self.black_tex)
                } else {
                    self.black_tex
                };
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            }

            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
        Ok(())
    }
}

// ------------------------------------------------------------ helpers GL

/// Compile un programme ISF et résout locations + unités des samplers.
fn compile_isf_program(gl: &Gl, sources: &IsfSources) -> Result<IsfProgram, CompositorError> {
    let program = link_program(gl, &sources.vertex, &sources.fragment)?;
    let mut uniforms = HashMap::new();
    let mut samplers = Vec::new();
    unsafe {
        gl.use_program(Some(program));
        let count = gl.get_active_uniforms(program);
        for i in 0..count {
            let Some(info) = gl.get_active_uniform(program, i) else {
                continue;
            };
            // Les tableaux remontent en "name[0]" : on garde le nom nu.
            let name = info.name.trim_end_matches("[0]").to_string();
            let Some(loc) = gl.get_uniform_location(program, &info.name) else {
                continue;
            };
            if info.utype == glow::SAMPLER_2D {
                let unit = samplers.len() as u32;
                gl.uniform_1_i32(Some(&loc), unit as i32);
                samplers.push((name.clone(), unit));
            }
            uniforms.insert(name, loc);
        }
        gl.use_program(None);
    }
    tracing::info!(
        target: "compositor",
        uniforms = uniforms.len(),
        samplers = samplers.len(),
        "programme ISF compilé"
    );
    Ok(IsfProgram {
        program,
        uniforms,
        samplers,
    })
}

/// Compile un shader ; en cas d'échec, retourne le log GLSL COMPLET (affiché
/// dans l'UI, jamais de panic).
fn compile_shader(
    gl: &Gl,
    stage: &'static str,
    shader_type: u32,
    src: &str,
) -> Result<<Gl as HasContext>::Shader, CompositorError> {
    unsafe {
        let shader = gl
            .create_shader(shader_type)
            .map_err(CompositorError::Resource)?;
        gl.shader_source(shader, src);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            tracing::error!(target: "compositor", stage, %log, "échec de compilation shader");
            return Err(CompositorError::ShaderCompile { stage, log });
        }
        Ok(shader)
    }
}

/// Compile + lie un programme complet (vertex + fragment).
fn link_program(gl: &Gl, vertex: &str, fragment: &str) -> Result<GlProgram, CompositorError> {
    let vs = compile_shader(gl, "vertex", glow::VERTEX_SHADER, vertex)?;
    let fs = match compile_shader(gl, "fragment", glow::FRAGMENT_SHADER, fragment) {
        Ok(fs) => fs,
        Err(e) => {
            unsafe { gl.delete_shader(vs) };
            return Err(e);
        }
    };
    unsafe {
        let program = match gl.create_program() {
            Ok(p) => p,
            Err(e) => {
                gl.delete_shader(vs);
                gl.delete_shader(fs);
                return Err(CompositorError::Resource(e));
            }
        };
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.link_program(program);
        gl.detach_shader(program, vs);
        gl.detach_shader(program, fs);
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            gl.delete_program(program);
            tracing::error!(target: "compositor", %log, "échec de link du programme");
            return Err(CompositorError::ProgramLink { log });
        }
        Ok(program)
    }
}

/// Crée une texture RGBA8 (filtrage linéaire, clamp aux bords).
fn create_texture_rgba(
    gl: &Gl,
    w: u32,
    h: u32,
    data: Option<&[u8]>,
) -> Result<GlTexture, CompositorError> {
    unsafe {
        let tex = gl.create_texture().map_err(CompositorError::Resource)?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(data),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);
        Ok(tex)
    }
}

/// Crée une cible de rendu offscreen (FBO + texture couleur RGBA8).
fn create_target(gl: &Gl, w: u32, h: u32) -> Result<RenderTarget, CompositorError> {
    let tex_id = create_texture_rgba(gl, w, h, None)?;
    unsafe {
        let fbo = match gl.create_framebuffer() {
            Ok(f) => f,
            Err(e) => {
                gl.delete_texture(tex_id);
                return Err(CompositorError::Resource(e));
            }
        };
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex_id),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        if status != glow::FRAMEBUFFER_COMPLETE {
            gl.delete_framebuffer(fbo);
            gl.delete_texture(tex_id);
            return Err(CompositorError::FramebufferIncomplete(status));
        }
        Ok(RenderTarget {
            fbo,
            tex: Tex2d { id: tex_id, w, h },
        })
    }
}

/// Libère une cible de rendu.
fn release_target(gl: &Gl, target: RenderTarget) {
    unsafe {
        gl.delete_framebuffer(target.fbo);
        gl.delete_texture(target.tex.id);
    }
}

/// Libère toutes les ressources GL d'un slice élagué (textures vidéo, PBO
/// d'upload, FBO matériaux).
fn release_slice(gl: &Gl, res: SliceRes) {
    for deck in res.decks {
        if let Some(tex) = deck.video {
            unsafe { gl.delete_texture(tex.id) };
        }
        if let Some(ring) = deck.upload {
            for buf in ring.bufs {
                unsafe { gl.delete_buffer(buf) };
            }
        }
        if let Some(slot) = deck.material {
            if let Some(target) = slot.target {
                release_target(gl, target);
            }
        }
    }
}

/// Crée une paire de PBO (upload ou readback, la cible est posée à l'usage).
fn create_pbo_pair(gl: &Gl) -> Result<[GlBuffer; 2], String> {
    unsafe {
        let a = gl.create_buffer()?;
        match gl.create_buffer() {
            Ok(b) => Ok([a, b]),
            Err(e) => {
                gl.delete_buffer(a);
                Err(e)
            }
        }
    }
}

/// Garantit le ring de PBO d'upload d'un deck. Retourne `None` si la
/// création a échoué (repli synchrone) ; l'échec n'est tenté qu'une fois par
/// process (`broken`), pas de spam de log par frame.
fn ensure_upload_ring<'a>(
    gl: &Gl,
    slot: &'a mut Option<UploadRing>,
    broken: &mut bool,
) -> Option<&'a mut UploadRing> {
    if slot.is_none() && !*broken {
        match create_pbo_pair(gl) {
            Ok(bufs) => *slot = Some(UploadRing { bufs, next: 0 }),
            Err(e) => {
                *broken = true;
                tracing::warn!(target: "compositor", %e, "PBO indisponibles, uploads synchrones");
            }
        }
    }
    slot.as_mut()
}

/// Crée un canal de lecture préview : deux PBO `GL_PIXEL_PACK_BUFFER`
/// dimensionnés pour `w`×`h` RGBA (stockage alloué une fois, `STREAM_READ`).
fn create_readback_channel(
    gl: &Gl,
    w: u32,
    h: u32,
    size: usize,
) -> Result<ReadbackChannel, String> {
    let bufs = create_pbo_pair(gl)?;
    unsafe {
        for buf in bufs {
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(buf));
            gl.buffer_data_size(glow::PIXEL_PACK_BUFFER, size as i32, glow::STREAM_READ);
        }
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
    }
    Ok(ReadbackChannel {
        bufs,
        write: 0,
        primed: [false, false],
        w,
        h,
    })
}
