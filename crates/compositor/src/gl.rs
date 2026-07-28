//! Implémentation GL du [`Compositor`] (glow). Aucune fenêtre ici : le
//! contexte est injecté par `app` et supposé courant à chaque appel.
//!
//! Perf : textures persistantes (stockage immuable `glTexStorage2D` quand
//! disponible), buffers réutilisés, uniform locations mises en cache — zéro
//! allocation dans `render_output` en régime établi. Upload vidéo par PBO
//! persistant mappé (fences par tranche) avec replis orphaning puis copie
//! synchrone ; frames BGRA acceptées telles quelles (`GL_BGRA`, format natif
//! Windows) sur GL desktop. Latence de présentation bornée par fences
//! ([`MAX_FRAMES_IN_FLIGHT`], métrique [`Compositor::frames_in_flight`]).
//! Chaque capacité est détectée à l'init ([`caps_from`], pur et testé) et
//! chaque chemin garde son repli.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use glow::HasContext;

use conduite_core::{MaterialId, ParamValue, PatternKind, SliceId};
use conduite_engine::{FrameRgba, PixelOrder};
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
type GlFence = <Gl as HasContext>::Fence;

/// Taille par défaut du rendu ISF quand `RENDERSIZE` n'a pas (encore) été
/// fourni par `app`.
const DEFAULT_MATERIAL_SIZE: (u32, u32) = (1280, 720);

/// Tranches du ring de PBO persistants d'upload (une frame en écriture, une
/// en transfert driver, une de marge).
const UPLOAD_RING_SLOTS: usize = 3;

/// Rendus vers une fenêtre de sortie autorisés « en vol » avant d'attendre
/// le GPU : borne la latence cue→écran à ~2 frames (pattern mpv).
const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// Attente unitaire sur une fence (2 ms) et nombre maximal d'itérations —
/// au pire ~16 ms puis on abandonne (jamais de gel du thread de rendu).
const FENCE_WAIT_NS: i32 = 2_000_000;
const FENCE_WAIT_TRIES: u32 = 8;

/// Capacités GL détectées à l'init (pur : décision testée sans contexte).
#[derive(Debug, Clone, Copy)]
struct GlCaps {
    /// `GL_BGRA` accepté comme format client d'upload (GL desktop ; absent
    /// du cœur GLES 3.0 → on reste en RGBA sur ES).
    bgra_upload: bool,
    /// `glTexStorage2D` (stockage immuable : anti-fragmentation driver sur
    /// les longues sessions) — GL ≥ 4.2 / ARB_texture_storage / GLES ≥ 3.0.
    tex_storage: bool,
    /// `glBufferStorage` + mapping persistant — GL ≥ 4.4 /
    /// ARB_buffer_storage / GLES + EXT_buffer_storage.
    buffer_storage: bool,
    /// Objets de synchronisation (`glFenceSync`) — GL ≥ 3.2 / GLES ≥ 3.0.
    fences: bool,
}

/// Décision de capacités à partir de la version et des extensions (pur).
fn caps_from(major: u32, minor: u32, embedded: bool, has_ext: &dyn Fn(&str) -> bool) -> GlCaps {
    let ver = (major, minor);
    if embedded {
        GlCaps {
            bgra_upload: false,
            tex_storage: ver >= (3, 0),
            buffer_storage: has_ext("GL_EXT_buffer_storage"),
            fences: ver >= (3, 0),
        }
    } else {
        GlCaps {
            // Format client GL_BGRA : présent en GL desktop depuis 1.2.
            bgra_upload: true,
            tex_storage: ver >= (4, 2) || has_ext("GL_ARB_texture_storage"),
            buffer_storage: ver >= (4, 4) || has_ext("GL_ARB_buffer_storage"),
            fences: ver >= (3, 2) || has_ext("GL_ARB_sync"),
        }
    }
}

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

/// Pointeur vers la zone mappée en persistant d'un PBO. La mémoire
/// appartient au buffer GL (valide tant que le buffer vit, libérée par
/// `glDeleteBuffers`) et n'est écrite que depuis le thread de rendu, contexte
/// courant — le marquage Send/Sync ne fait que préserver les auto-traits du
/// `Compositor`.
struct MappedPtr(*mut u8);
// SAFETY : voir ci-dessus — usage confiné au thread qui détient le contexte.
unsafe impl Send for MappedPtr {}
unsafe impl Sync for MappedPtr {}

/// Ring de PBO persistant mappé (`ARB_buffer_storage`) : un seul buffer de
/// [`UPLOAD_RING_SLOTS`] tranches, mappé UNE FOIS en écriture
/// (`MAP_PERSISTENT_BIT | MAP_COHERENT_BIT`). La frame est copiée dans la
/// tranche libre (mémoire visible GPU, zéro allocation driver), puis
/// `glTexSubImage2D` lit depuis l'offset ; une fence par tranche garantit
/// que le driver a fini avant réécriture.
///
/// NOTE (chemin restant, non fait) : faire écrire le thread LECTEUR ffmpeg
/// directement dans la tranche mappée (`read_exact` pipe → mémoire visible
/// GPU, ~1 memcpy de moins par frame) exigerait de casser le découplage
/// actuel `BufferPool` → `FrameRing` → upload : le lecteur (crate engine,
/// sans GL) devrait emprunter des tranches au compositor et les rendre
/// synchronisées par fence à travers `app`, y compris pendant seek/replays
/// où les frames en vol sont jetées. C'est une refonte du contrat
/// engine↔app↔compositor — à traiter avec le chantier HAP natif, qui
/// réécrira de toute façon ce chemin (glCompressedTexSubImage2D + PBO).
struct PersistentRing {
    buf: GlBuffer,
    ptr: MappedPtr,
    slot_size: usize,
    fences: [Option<GlFence>; UPLOAD_RING_SLOTS],
    /// Tranche du prochain upload.
    next: usize,
}

/// Chemin d'upload d'un deck, par capacité décroissante :
/// - `Persistent` : PBO persistant mappé + fences (GL 4.4 / ARB_buffer_storage) ;
/// - `Orphan` : ring de 2 PBO orphanés (`glBufferData(NULL)` + copie), le
///   chemin historique, disponible en GL 3.3 core ET GLES 3.0.
enum UploadRing {
    Persistent(PersistentRing),
    Orphan {
        bufs: [GlBuffer; 2],
        /// Index du PBO utilisé pour le prochain upload.
        next: usize,
    },
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
    /// Mire ident : libellé « nom — résolution » de la sortie.
    pattern_px: Option<GlUniformLocation>,
    ident_len: Option<GlUniformLocation>,
    ident_text: Option<GlUniformLocation>,
}

/// Le compositor : tout le GL du produit. Créé avec un contexte partagé
/// entre les fenêtres de sortie ; `app` rend le bon contexte courant avant
/// chaque appel.
pub struct Compositor {
    gl: Arc<Gl>,
    glsl: GlslVersion,
    caps: GlCaps,
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
    /// Création de PBO persistant en échec : repli orphané, loggé une fois.
    persistent_pbo_broken: bool,
    /// Framebuffer cible courant (None = framebuffer par défaut).
    current_target: Option<GlFramebuffer>,
    /// Scratch de tri par z, réutilisé chaque frame.
    z_indices: Vec<usize>,
    /// Fences des rendus de sortie soumis, plus ancien en tête (borne la
    /// latence de présentation à [`MAX_FRAMES_IN_FLIGHT`]).
    frame_fences: VecDeque<GlFence>,
    /// `glFenceSync` en échec : limiteur désactivé, loggé une seule fois.
    fence_broken: bool,
    /// Dernière mesure du nombre de rendus en vol (métrique HUD santé).
    frames_in_flight: usize,
    /// Frame BGRA reçue sans support GL_BGRA : loggé une seule fois.
    bgra_mismatch_logged: bool,
    /// Libellé de la mire ident (« nom — résolution » de la sortie), en
    /// indices de glyphes — posé par [`Compositor::set_ident_label`] avant le
    /// rendu de chaque sortie ; 0 glyphe = numéro seul (compat).
    ident_text: [i32; crate::font::IDENT_TEXT_MAX],
    ident_len: i32,
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
        let extensions = gl.supported_extensions();
        let caps = caps_from(version.major, version.minor, version.is_embedded, &|name| {
            extensions.contains(name)
        });
        tracing::info!(
            target: "compositor",
            major = version.major,
            minor = version.minor,
            embedded = version.is_embedded,
            ?glsl,
            bgra = caps.bgra_upload,
            tex_storage = caps.tex_storage,
            buffer_storage = caps.buffer_storage,
            fences = caps.fences,
            "initialisation du compositor"
        );
        // Format natif Windows : demande à ffmpeg de sortir du BGRA (upload
        // GL sans swizzle driver). Sur GLES, GL_BGRA n'existe pas en cœur :
        // on reste en RGBA. Chaque frame porte son ordre — pas de course.
        conduite_engine::set_decode_bgra(caps.bgra_upload);

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
                pattern_px: gl.get_uniform_location(composite, "u_pattern_px"),
                ident_len: gl.get_uniform_location(composite, "u_ident_len"),
                // Certains drivers exposent les tableaux sous "nom[0]".
                ident_text: gl
                    .get_uniform_location(composite, "u_ident_text")
                    .or_else(|| gl.get_uniform_location(composite, "u_ident_text[0]")),
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
        let black_tex = create_texture_rgba(&gl, 1, 1, Some(&[0, 0, 0, 255]), caps.tex_storage)?;

        Ok(Self {
            gl,
            glsl,
            caps,
            composite,
            locs,
            quad_vao,
            black_tex,
            slices: HashMap::new(),
            isf_cache: ProgramCache::default(),
            preview: None,
            readbacks: HashMap::new(),
            pbo_broken: false,
            persistent_pbo_broken: false,
            current_target: None,
            z_indices: Vec::with_capacity(32),
            frame_fences: VecDeque::with_capacity(MAX_FRAMES_IN_FLIGHT + 1),
            fence_broken: false,
            frames_in_flight: 0,
            bgra_mismatch_logged: false,
            ident_text: [0; crate::font::IDENT_TEXT_MAX],
            ident_len: 0,
        })
    }

    /// Pose le libellé de la mire [`PatternKind::Ident`] : nom + résolution
    /// de la SORTIE (ex. « PRINCIPAL — 1920×1080 »). À appeler avant le rendu
    /// de chaque sortie qui affiche une mire ident (encodage pur, zéro
    /// allocation) ; sans appel, la mire n'affiche que le numéro (compat).
    pub fn set_ident_label(&mut self, name: &str, width: u32, height: u32) {
        self.ident_len = crate::font::encode_output_ident(name, width, height, &mut self.ident_text);
    }

    /// Efface le libellé de la mire ident (retour au numéro seul).
    pub fn clear_ident_label(&mut self) {
        self.ident_len = 0;
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
                match create_texture_rgba(&self.gl, 1, 1, Some(&[0, 0, 0, 255]), self.caps.tex_storage) {
                    Ok(id) => deck.video = Some(Tex2d { id, w: 1, h: 1 }),
                    Err(e) => {
                        tracing::error!(target: "compositor", slice, %e, "création de texture");
                    }
                }
            }
        }
    }

    /// Upload d'une frame décodée dans la texture du deck. La texture est
    /// réutilisée et recréée seulement si les dimensions changent (stockage
    /// immuable `glTexStorage2D` quand disponible). Chemins d'upload, par
    /// capacité décroissante : ring de 3 tranches de PBO persistant mappé +
    /// fences (copie directe en mémoire visible GPU), ring de 2 PBO orphanés,
    /// `glTexSubImage2D` direct. Le format client suit l'ordre de canaux de
    /// la frame (`GL_BGRA` natif Windows quand le moteur décode en BGRA).
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
        let src_format = match f.pixel_order() {
            PixelOrder::Bgra if self.caps.bgra_upload => glow::BGRA,
            PixelOrder::Bgra => {
                // Ne devrait pas arriver : le moteur ne produit du BGRA que
                // si ce compositor l'a activé. Canaux permutés plutôt que rien.
                if !self.bgra_mismatch_logged {
                    self.bgra_mismatch_logged = true;
                    tracing::error!(
                        target: "compositor",
                        "frame BGRA sans support GL_BGRA : upload en RGBA (canaux permutés)"
                    );
                }
                glow::RGBA
            }
            PixelOrder::Rgba => glow::RGBA,
        };
        self.ensure_slice_textures(slice);
        let gl = &self.gl;
        let Some(res) = self.slices.get_mut(&slice) else {
            return;
        };
        let deck_res = &mut res.decks[deck.index()];
        let Some(tex) = deck_res.video.as_mut() else {
            return; // création ratée, déjà loggé
        };

        if tex.w != f.width || tex.h != f.height {
            // Dimensions changées : chemin rare. Le stockage étant immuable
            // (tex storage), on RECRÉE la texture ; le ring d'upload est
            // libéré (tranches dimensionnées pour l'ancienne taille).
            if let Some(ring) = deck_res.upload.take() {
                release_upload_ring(gl, ring);
            }
            match create_texture_rgba(gl, f.width, f.height, None, self.caps.tex_storage) {
                Ok(id) => {
                    unsafe { gl.delete_texture(tex.id) };
                    *tex = Tex2d { id, w: f.width, h: f.height };
                }
                Err(e) => {
                    tracing::error!(target: "compositor", slice, %e, "recréation de texture");
                    return;
                }
            }
        }

        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(tex.id));
            match ensure_upload_ring(
                gl,
                &self.caps,
                &mut deck_res.upload,
                &mut self.pbo_broken,
                &mut self.persistent_pbo_broken,
                expected,
            ) {
                Some(UploadRing::Persistent(ring)) => {
                    // Chemin premium : copie directe dans la tranche mappée
                    // (mémoire visible GPU), fence par tranche.
                    let slot = ring.next;
                    ring.next = (slot + 1) % UPLOAD_RING_SLOTS;
                    if let Some(fence) = ring.fences[slot].take() {
                        wait_fence_bounded(gl, fence);
                    }
                    std::ptr::copy_nonoverlapping(
                        f.data.as_ptr(),
                        ring.ptr.0.add(slot * ring.slot_size),
                        expected,
                    );
                    gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(ring.buf));
                    gl.tex_sub_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        0,
                        0,
                        f.width as i32,
                        f.height as i32,
                        src_format,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::BufferOffset((slot * ring.slot_size) as u32),
                    );
                    ring.fences[slot] = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0).ok();
                    gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
                }
                Some(UploadRing::Orphan { bufs, next }) => {
                    // PBO orphané puis rempli, glTexSubImage2D lit depuis le
                    // buffer — le transfert devient asynchrone.
                    let buf = bufs[*next];
                    *next ^= 1;
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
                        src_format,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::BufferOffset(0),
                    );
                    gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
                }
                None => {
                    // Repli sans PBO : copie client synchrone (comportement
                    // historique, toujours correct).
                    gl.tex_sub_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        0,
                        0,
                        f.width as i32,
                        f.height as i32,
                        src_format,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(&f.data[..expected])),
                    );
                }
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

        // Libellé de la mire ident : uniforms posés une fois par sortie,
        // SEULEMENT si une mire ident est affichée (jamais de coût sur le
        // chemin nominal). Toujours reposés quand elle l'est : le programme
        // est partagé entre les sorties (libellés différents).
        if out
            .slices
            .iter()
            .any(|s| s.pattern == Some(PatternKind::Ident))
        {
            let gl = &self.gl;
            unsafe {
                gl.uniform_2_f32(
                    self.locs.pattern_px.as_ref(),
                    out.output_size.0 as f32,
                    out.output_size.1 as f32,
                );
                gl.uniform_1_i32(self.locs.ident_len.as_ref(), self.ident_len);
                if self.ident_len > 0 {
                    gl.uniform_1_i32_slice(self.locs.ident_text.as_ref(), &self.ident_text);
                }
            }
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
        if self.current_target.is_none() {
            self.limit_frame_latency();
        }
        Ok(())
    }

    /// Borne la latence de présentation : une fence par rendu vers une
    /// fenêtre de sortie ; au-delà de [`MAX_FRAMES_IN_FLIGHT`] rendus non
    /// terminés par le GPU, on attend (court, borné) le plus ancien. Sans
    /// cela, un driver peut mettre plusieurs frames en file et la latence
    /// cue→écran dérive. Pattern mpv. No-op si les fences sont indisponibles.
    fn limit_frame_latency(&mut self) {
        if self.fence_broken || !self.caps.fences {
            return;
        }
        let gl = &self.gl;
        unsafe {
            match gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) {
                Ok(fence) => self.frame_fences.push_back(fence),
                Err(e) => {
                    self.fence_broken = true;
                    tracing::warn!(
                        target: "compositor",
                        %e,
                        "glFenceSync indisponible, limiteur de latence désactivé"
                    );
                    return;
                }
            }
            // Purge non bloquante des rendus déjà terminés.
            while let Some(&fence) = self.frame_fences.front() {
                match gl.client_wait_sync(fence, 0, 0) {
                    glow::ALREADY_SIGNALED | glow::CONDITION_SATISFIED | glow::WAIT_FAILED => {
                        gl.delete_sync(fence);
                        self.frame_fences.pop_front();
                    }
                    _ => break,
                }
            }
        }
        // Trop de rendus en vol : attente courte et bornée du plus ancien.
        while self.frame_fences.len() > MAX_FRAMES_IN_FLIGHT {
            let Some(fence) = self.frame_fences.pop_front() else {
                break;
            };
            wait_fence_bounded(&self.gl, fence);
        }
        self.frames_in_flight = self.frame_fences.len();
    }

    /// Nombre de rendus de sortie soumis au GPU et pas encore terminés à la
    /// dernière mesure (0..=[`MAX_FRAMES_IN_FLIGHT`]) — métrique pour le HUD
    /// santé. Toujours 0 si les fences sont indisponibles.
    pub fn frames_in_flight(&self) -> usize {
        self.frames_in_flight
    }

    /// Rend une mire plein cadre dans le framebuffer courant (calage global,
    /// bouton « identifier » d'une sortie). `ident_num` est le numéro affiché
    /// par la mire [`PatternKind::Ident`] ; son libellé « nom — résolution »
    /// est celui posé par [`Compositor::set_ident_label`].
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
            gl.uniform_2_f32(
                locs.pattern_px.as_ref(),
                output_size.0 as f32,
                output_size.1 as f32,
            );
            gl.uniform_1_i32(locs.ident_len.as_ref(), self.ident_len);
            if self.ident_len > 0 {
                gl.uniform_1_i32_slice(locs.ident_text.as_ref(), &self.ident_text);
            }

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.black_tex));
            gl.active_texture(glow::TEXTURE0 + 1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.black_tex));

            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
        if self.current_target.is_none() {
            self.limit_frame_latency();
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
            self.preview = Some(create_target(&self.gl, w, h, self.caps.tex_storage)?);
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
        let tex_storage = self.caps.tex_storage;
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
            slot.target = Some(create_target(gl, w, h, tex_storage)?);
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
/// `immutable` : stockage `glTexStorage2D` (1 niveau) — l'allocation est
/// définitive, anti-fragmentation driver sur les sessions de 8 h ; la
/// texture doit être RECRÉÉE pour changer de taille. `data` (optionnel) est
/// interprété en RGBA.
fn create_texture_rgba(
    gl: &Gl,
    w: u32,
    h: u32,
    data: Option<&[u8]>,
    immutable: bool,
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
        if immutable {
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, w as i32, h as i32);
            if let Some(data) = data {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(data)),
                );
            }
        } else {
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
        }
        gl.bind_texture(glow::TEXTURE_2D, None);
        Ok(tex)
    }
}

/// Crée une cible de rendu offscreen (FBO + texture couleur RGBA8).
fn create_target(
    gl: &Gl,
    w: u32,
    h: u32,
    immutable: bool,
) -> Result<RenderTarget, CompositorError> {
    let tex_id = create_texture_rgba(gl, w, h, None, immutable)?;
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
            release_upload_ring(gl, ring);
        }
        if let Some(slot) = deck.material {
            if let Some(target) = slot.target {
                release_target(gl, target);
            }
        }
    }
}

/// Libère un ring d'upload (buffers + fences). Un PBO persistant mappé est
/// démappé implicitement par sa suppression (spécification GL).
fn release_upload_ring(gl: &Gl, ring: UploadRing) {
    unsafe {
        match ring {
            UploadRing::Persistent(ring) => {
                for fence in ring.fences.into_iter().flatten() {
                    gl.delete_sync(fence);
                }
                gl.delete_buffer(ring.buf);
            }
            UploadRing::Orphan { bufs, .. } => {
                for buf in bufs {
                    gl.delete_buffer(buf);
                }
            }
        }
    }
}

/// Attend (borné) qu'une fence soit signalée, puis la supprime. Premier test
/// à timeout 0 (cas nominal : déjà signalée), puis attentes courtes de
/// [`FENCE_WAIT_NS`] avec flush ; après [`FENCE_WAIT_TRIES`] itérations on
/// abandonne — jamais de gel du thread de rendu.
fn wait_fence_bounded(gl: &Gl, fence: GlFence) {
    unsafe {
        let mut tries = 0u32;
        loop {
            let (flags, timeout) = if tries == 0 {
                (0, 0)
            } else {
                (glow::SYNC_FLUSH_COMMANDS_BIT, FENCE_WAIT_NS)
            };
            match gl.client_wait_sync(fence, flags, timeout) {
                glow::ALREADY_SIGNALED | glow::CONDITION_SATISFIED => break,
                glow::WAIT_FAILED => break, // erreur GL : ne pas boucler
                _ => {
                    tries += 1;
                    if tries > FENCE_WAIT_TRIES {
                        tracing::warn!(
                            target: "compositor",
                            "fence GPU toujours en attente après {} ms, poursuite",
                            (FENCE_WAIT_NS as i64 * FENCE_WAIT_TRIES as i64) / 1_000_000
                        );
                        break;
                    }
                }
            }
        }
        gl.delete_sync(fence);
    }
}

/// Crée un ring de PBO persistant mappé : un buffer de
/// `slot_size × UPLOAD_RING_SLOTS` octets en stockage immuable
/// (`glBufferStorage`), mappé une fois en écriture persistante cohérente.
fn create_persistent_ring(gl: &Gl, slot_size: usize) -> Result<PersistentRing, String> {
    let total = slot_size
        .checked_mul(UPLOAD_RING_SLOTS)
        .filter(|t| *t <= i32::MAX as usize)
        .ok_or_else(|| format!("taille de ring d'upload invalide ({slot_size} octets/tranche)"))?;
    let flags = glow::MAP_WRITE_BIT | glow::MAP_PERSISTENT_BIT | glow::MAP_COHERENT_BIT;
    unsafe {
        let buf = gl.create_buffer()?;
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(buf));
        gl.buffer_storage(glow::PIXEL_UNPACK_BUFFER, total as i32, None, flags);
        let err = gl.get_error();
        if err != glow::NO_ERROR {
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            gl.delete_buffer(buf);
            return Err(format!("glBufferStorage : erreur GL 0x{err:x}"));
        }
        let ptr = gl.map_buffer_range(glow::PIXEL_UNPACK_BUFFER, 0, total as i32, flags);
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        if ptr.is_null() {
            gl.delete_buffer(buf);
            return Err("glMapBufferRange persistant : pointeur nul".into());
        }
        Ok(PersistentRing {
            buf,
            ptr: MappedPtr(ptr),
            slot_size,
            fences: std::array::from_fn(|_| None),
            next: 0,
        })
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

/// Garantit le ring de PBO d'upload d'un deck : persistant mappé si le
/// contexte le permet (`buffer_storage` + fences), sinon paire orphanée.
/// Retourne `None` si la création a échoué (repli synchrone) ; l'échec n'est
/// tenté qu'une fois par process (`broken`), pas de spam de log par frame.
fn ensure_upload_ring<'a>(
    gl: &Gl,
    caps: &GlCaps,
    slot: &'a mut Option<UploadRing>,
    broken: &mut bool,
    persistent_broken: &mut bool,
    slot_size: usize,
) -> Option<&'a mut UploadRing> {
    if slot.is_none() && !*broken {
        if caps.buffer_storage && caps.fences && !*persistent_broken {
            match create_persistent_ring(gl, slot_size) {
                Ok(ring) => {
                    tracing::debug!(
                        target: "compositor",
                        slot_size,
                        "ring d'upload persistant mappé ({} tranches)",
                        UPLOAD_RING_SLOTS
                    );
                    *slot = Some(UploadRing::Persistent(ring));
                }
                Err(e) => {
                    // Sticky : plus de tentative persistante, mais le repli
                    // orphané (ci-dessous) reste disponible.
                    *persistent_broken = true;
                    tracing::warn!(
                        target: "compositor",
                        %e,
                        "PBO persistant indisponible, repli sur l'orphaning"
                    );
                }
            }
        }
        if slot.is_none() {
            match create_pbo_pair(gl) {
                Ok(bufs) => *slot = Some(UploadRing::Orphan { bufs, next: 0 }),
                Err(e) => {
                    *broken = true;
                    tracing::warn!(target: "compositor", %e, "PBO indisponibles, uploads synchrones");
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn no_ext(_: &str) -> bool {
        false
    }

    #[test]
    fn caps_gl33_desktop_sans_extensions() {
        // Cible minimale du produit : GL 3.3 core nu.
        let c = caps_from(3, 3, false, &no_ext);
        assert!(c.bgra_upload, "GL_BGRA est du GL desktop de base");
        assert!(!c.tex_storage);
        assert!(!c.buffer_storage);
        assert!(c.fences, "sync objects depuis GL 3.2");
    }

    #[test]
    fn caps_gl33_desktop_avec_extensions_arb() {
        // Cas fréquent : driver GL 3.3 exposant les ARB modernes.
        let has = |name: &str| {
            matches!(name, "GL_ARB_texture_storage" | "GL_ARB_buffer_storage")
        };
        let c = caps_from(3, 3, false, &has);
        assert!(c.tex_storage);
        assert!(c.buffer_storage);
    }

    #[test]
    fn caps_gl46_desktop_tout_en_version() {
        let c = caps_from(4, 6, false, &no_ext);
        assert!(c.bgra_upload && c.tex_storage && c.buffer_storage && c.fences);
    }

    #[test]
    fn caps_gles30_jamais_de_bgra() {
        // GLES 3.0 (Raspberry Pi) : GL_BGRA absent du cœur — RGBA conservé.
        let c = caps_from(3, 0, true, &no_ext);
        assert!(!c.bgra_upload);
        assert!(c.tex_storage, "glTexStorage2D est du cœur GLES 3.0");
        assert!(!c.buffer_storage);
        assert!(c.fences);
        // Même avec EXT_buffer_storage : BGRA reste exclu.
        let c = caps_from(3, 1, true, &|n| n == "GL_EXT_buffer_storage");
        assert!(!c.bgra_upload);
        assert!(c.buffer_storage);
    }

    #[test]
    fn caps_gles2_rien_de_moderne() {
        let c = caps_from(2, 0, true, &no_ext);
        assert!(!c.bgra_upload && !c.tex_storage && !c.buffer_storage && !c.fences);
    }
}
