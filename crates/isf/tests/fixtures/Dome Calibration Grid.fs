/*{
  "DESCRIPTION": "Grille de calibration dome-native : grille az/el, cardinaux N-E-S-W, spring-line avec tilt, safe-area IMERSA (az ±50°, el 10-60°), croix zénith. Convention : bas de l'image = avant du dôme. Pack Sources Dome-Native.",
  "CREDIT": "Pack Sources Dome-Native — v1.1.0",
  "VSN": "1.1.0",
  "ISFVSN": "2",
  "CATEGORIES": [
    "Generator",
    "Dome",
    "Utility"
  ],
  "INPUTS": [
    {
      "NAME": "orientation",
      "TYPE": "float",
      "MIN": -180.0,
      "MAX": 180.0,
      "DEFAULT": 0.0,
      "LABEL": "Orientation (yaw °)"
    },
    {
      "NAME": "domeTilt",
      "TYPE": "float",
      "MIN": 0.0,
      "MAX": 30.0,
      "DEFAULT": 0.0,
      "LABEL": "Tilt dôme (°)"
    },
    {
      "NAME": "maskFeather",
      "TYPE": "float",
      "MIN": 0.0,
      "MAX": 0.5,
      "DEFAULT": 0.02,
      "LABEL": "Masque — feather"
    },
    {
      "NAME": "intensity",
      "TYPE": "float",
      "MIN": 0.0,
      "MAX": 2.0,
      "DEFAULT": 1.0,
      "LABEL": "Intensité"
    },
    {
      "NAME": "lineColor",
      "TYPE": "color",
      "DEFAULT": [
        1.0,
        1.0,
        1.0,
        1.0
      ],
      "LABEL": "Couleur grille"
    },
    {
      "NAME": "accentColor",
      "TYPE": "color",
      "DEFAULT": [
        0.0,
        0.8,
        1.0,
        1.0
      ],
      "LABEL": "Couleur accent"
    },
    {
      "NAME": "gridStepAz",
      "TYPE": "float",
      "MIN": 5.0,
      "MAX": 45.0,
      "DEFAULT": 15.0,
      "LABEL": "Pas azimut (°)"
    },
    {
      "NAME": "gridStepEl",
      "TYPE": "float",
      "MIN": 5.0,
      "MAX": 30.0,
      "DEFAULT": 10.0,
      "LABEL": "Pas élévation (°)"
    },
    {
      "NAME": "lineWidth",
      "TYPE": "float",
      "MIN": 0.0008,
      "MAX": 0.008,
      "DEFAULT": 0.002,
      "LABEL": "Épaisseur"
    },
    {
      "NAME": "showSafeArea",
      "TYPE": "bool",
      "DEFAULT": 1.0,
      "LABEL": "Safe-area IMERSA"
    },
    {
      "NAME": "showCardinals",
      "TYPE": "bool",
      "DEFAULT": 1.0,
      "LABEL": "Cardinaux NESW"
    },
    {
      "NAME": "showZenith",
      "TYPE": "bool",
      "DEFAULT": 1.0,
      "LABEL": "Croix zénith"
    },
    {
      "NAME": "showSpringLine",
      "TYPE": "bool",
      "DEFAULT": 1.0,
      "LABEL": "Spring-line"
    }
  ]
}*/

// ============================================================
// dome.glsl — bibliothèque commune du pack « Sources Dome-Native »
// v0.1 — concaténée en tête de chaque shader par build.py
//
// Projection : équidistante azimutale (domemaster / fisheye 180°)
// Conventions :
//   - bas de l'image  = AVANT du dôme  (az = 0)
//   - droite de l'image = EST / droite du spectateur (az = +90°)
//   - centre de l'image = zénith (el = PI/2)
//   - bord du cercle    = spring-line du dôme (el = 0)
// Unités : angles en radians. az ∈ [-PI, PI], el ∈ [0, PI/2].
// Espace écran : p ∈ [-1, 1]², rayon image = 1. Toutes les
// épaisseurs de trait sont exprimées dans cette unité.
//
// Architecture STATELESS : aucun feedback, aucun buffer persistant.
// Tout est fonction analytique de TIME + hash (contrainte Wire :
// buffers multipass quantifiés [0,1], voir spec §2).
// ============================================================

const float PI      = 3.14159265358979;
const float TAU     = 6.28318530717959;
const float HALF_PI = 1.57079632679490;

// ---------- angles ------------------------------------------------

// replie un angle dans [-PI, PI]
float wrapAngle(float a) { return mod(a + PI, TAU) - PI; }

// répétition N-fold en azimut (replie az dans un secteur de TAU/n centré sur 0)
float azRepeat(float az, float n) {
    if (n <= 1.0) return az;
    float s = TAU / n;
    return mod(az + 0.5 * s, s) - 0.5 * s;
}

mat2 rot2(float a) { float c = cos(a), s = sin(a); return mat2(c, s, -s, c); }

// ---------- espace écran <-> espace dôme --------------------------

vec2 uvToScreen(vec2 uv) { return (uv - 0.5) * 2.0; }

// uv -> (azimut, élévation)
vec2 uvToDome(vec2 uv) {
    vec2 p = uvToScreen(uv);
    float r = length(p);
    float el = (1.0 - r) * HALF_PI;
    // atan(0,0) est indéfini en GLSL : au pixel exact du zénith, az = 0
    float az = (r > 1e-6) ? atan(p.x, -p.y) : 0.0;   // az = 0 en bas (avant)
    return vec2(az, el);
}

// (az, el) -> position écran dans [-1, 1]²
vec2 domeToScreen(float az, float el) {
    float r = 1.0 - el / HALF_PI;
    return r * vec2(sin(az), -cos(az));
}

vec2 domeToUv(float az, float el) { return domeToScreen(az, el) * 0.5 + 0.5; }

// applique l'orientation globale (yaw en degrés, param commun `orientation`)
vec2 applyOrient(vec2 azel, float yawDeg) {
    azel.x = wrapAngle(azel.x - radians(yawDeg));
    return azel;
}

// direction 3D unitaire du point (az, el) — y = avant, z = zénith
vec3 domeDir(vec2 azel) {
    float ce = cos(azel.y);
    return vec3(ce * sin(azel.x), ce * cos(azel.x), sin(azel.y));
}

// base orthonormée tangente au point (az, el) vue depuis le centre :
// c = direction du point, u = droite du spectateur (azimut+), v = vers le zénith
void domeBasis(float az, float el, out vec3 c, out vec3 u, out vec3 v) {
    c = domeDir(vec2(az, el));
    u = vec3(cos(az), -sin(az), 0.0);
    v = cross(u, c);
}

// rotation de p autour de l'axe unitaire ax (Rodrigues)
vec3 rotAxis(vec3 p, vec3 ax, float a) {
    float c = cos(a), s = sin(a);
    return p * c + cross(ax, p) * s + ax * dot(ax, p) * (1.0 - c);
}

// ---------- dôme physique -----------------------------------------

// élévation (radians) de l'horizon vrai à l'azimut az pour un dôme
// incliné de `tilt` radians vers l'avant. tilt = 0 -> horizon = bord.
float springLineEl(float az, float tilt) {
    return atan(tan(tilt) * cos(az));
}

// masque circulaire du dôme avec feather de bord
float domeMask(vec2 uv, float feather) {
    float r = length(uvToScreen(uv));
    feather = max(feather, 1e-4);
    return 1.0 - smoothstep(1.0 - feather, 1.0, r);
}

// ---------- SDF en espace polaire dôme ----------------------------
// Toutes retournent une distance ÉCRAN (rayon image = 1), compensée
// du jacobien de la projection => épaisseur constante à l'écran.

// distance à un méridien d'azimut az0
float sdMeridian(vec2 azel, float az0) {
    float r = 1.0 - azel.y / HALF_PI;
    return abs(wrapAngle(azel.x - az0)) * max(r, 1e-4);
}

// distance à un parallèle d'élévation el0 (cercle centré zénith)
float sdParallel(vec2 azel, float el0) {
    return abs(el0 - azel.y) / HALF_PI;
}

// segment de méridien limité en élévation [elA, elB]
float sdMeridianSeg(vec2 azel, float az0, float elA, float elB) {
    float elc = clamp(azel.y, elA, elB);
    float r  = 1.0 - azel.y / HALF_PI;
    float dt = wrapAngle(azel.x - az0) * max(r, 1e-4);
    float dr = (azel.y - elc) / HALF_PI;
    return length(vec2(dt, dr));
}

// arc de parallèle limité en azimut [azA, azB] (azA < azB, autour de 0)
float sdParallelSeg(vec2 azel, float el0, float azA, float azB) {
    float mid = 0.5 * (azA + azB);
    float hw  = 0.5 * (azB - azA);
    float azc = clamp(wrapAngle(azel.x - mid), -hw, hw) + mid;
    float r  = 1.0 - el0 / HALF_PI;
    float dt = wrapAngle(azel.x - azc) * max(r, 1e-4);
    float dr = (azel.y - el0) / HALF_PI;
    return length(vec2(dt, dr));
}

// segment 2D générique en espace écran
float sdSegment2(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a, ba = b - a;
    float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

// distance ANGULAIRE au grand cercle de pôle n (unitaire)
float angGreatCircle(vec2 azel, vec3 n) {
    return abs(asin(clamp(dot(domeDir(azel), n), -1.0, 1.0)));
}

// conversion approchée angulaire -> écran au point courant
// (moyenne des échelles radiale et tangentielle ; suffisant pour des
// traits fins, à raffiner par gradient si besoin d'exactitude)
float angToScreen(vec2 azel, float ang) {
    float r    = max(1.0 - azel.y / HALF_PI, 1e-3);
    float sTan = r / max(cos(azel.y), 1e-3);
    float sRad = 2.0 / PI;
    return ang * mix(sRad, sTan, 0.5);
}

// ---------- rendu de traits ---------------------------------------

// trait plein antialiasé : d = distance écran, halfW = demi-épaisseur
float strokeMask(float d, float halfW, float aa) {
    return 1.0 - smoothstep(halfW - aa, halfW + aa, d);
}

// halo gaussien autour d'une ligne
float glowMask(float d, float radius) {
    return exp(-d * d / max(radius * radius, 1e-6));
}

// ---------- hash / noise (stateless) ------------------------------

float hash11(float p) {
    p = fract(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return fract(p);
}

float hash21(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

float hash31(vec3 p3) {
    p3 = fract(p3 * 0.1031);
    p3 += dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

vec2 hash22(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * vec3(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.xx + p3.yz) * p3.zy);
}

float noise2(vec2 x) {
    vec2 i = floor(x), f = fract(x);
    f = f * f * (3.0 - 2.0 * f);
    float a = hash21(i);
    float b = hash21(i + vec2(1.0, 0.0));
    float c = hash21(i + vec2(0.0, 1.0));
    float d = hash21(i + vec2(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

float noise3(vec3 x) {
    vec3 i = floor(x), f = fract(x);
    f = f * f * (3.0 - 2.0 * f);
    float n000 = hash31(i);
    float n100 = hash31(i + vec3(1.0, 0.0, 0.0));
    float n010 = hash31(i + vec3(0.0, 1.0, 0.0));
    float n110 = hash31(i + vec3(1.0, 1.0, 0.0));
    float n001 = hash31(i + vec3(0.0, 0.0, 1.0));
    float n101 = hash31(i + vec3(1.0, 0.0, 1.0));
    float n011 = hash31(i + vec3(0.0, 1.0, 1.0));
    float n111 = hash31(i + vec3(1.0, 1.0, 1.0));
    return mix(mix(mix(n000, n100, f.x), mix(n010, n110, f.x), f.y),
               mix(mix(n001, n101, f.x), mix(n011, n111, f.x), f.y), f.z);
}

float fbm3(vec3 p) {
    float v = 0.0, amp = 0.5;
    for (int i = 0; i < 4; i++) {
        v += amp * noise3(p);
        p = p * 2.03 + vec3(17.1, 9.2, 3.7);
        amp *= 0.5;
    }
    return v;
}

// ---------- couleur / palettes ------------------------------------

float luma(vec3 c) { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }

vec3 rgb2hsv(vec3 c) {
    vec4 K = vec4(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    vec4 p = mix(vec4(c.bg, K.wz), vec4(c.gb, K.xy), step(c.b, c.g));
    vec4 q = mix(vec4(p.xyw, c.r), vec4(c.r, p.yzx), step(p.x, c.r));
    float d = q.x - min(q.w, q.y);
    float e = 1.0e-10;
    return vec3(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

vec3 hsv2rgb(vec3 c) {
    vec4 K = vec4(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    vec3 p = abs(fract(c.xxx + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, 0.0, 1.0), c.y);
}

vec3 rgb2oklab(vec3 c) {
    float l = 0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b;
    float m = 0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b;
    float s = 0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b;
    l = pow(max(l, 0.0), 1.0 / 3.0);
    m = pow(max(m, 0.0), 1.0 / 3.0);
    s = pow(max(s, 0.0), 1.0 / 3.0);
    return vec3(0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
                1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
                0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s);
}

vec3 oklab2rgb(vec3 c) {
    float l = c.x + 0.3963377774 * c.y + 0.2158037573 * c.z;
    float m = c.x - 0.1055613458 * c.y - 0.0638541728 * c.z;
    float s = c.x - 0.0894841775 * c.y - 1.2914855480 * c.z;
    l = l * l * l; m = m * m * m; s = s * s * s;
    return clamp(vec3( 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
                      -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
                      -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s), 0.0, 1.0);
}

// mix entre deux couleurs selon le mode palette (0=RGB, 1=HSV, 2=OKLab)
vec3 palMix(vec3 a, vec3 b, float t, int mode) {
    if (mode == 1) {
        vec3 ha = rgb2hsv(a), hb = rgb2hsv(b);
        float dh = hb.x - ha.x;
        dh -= floor(dh + 0.5);                 // chemin de teinte le plus court
        return hsv2rgb(vec3(ha.x + dh * t, mix(ha.yz, hb.yz, t)));
    } else if (mode == 2) {
        return oklab2rgb(mix(rgb2oklab(a), rgb2oklab(b), t));
    }
    return mix(a, b, t);
}

// palette cyclique 2-4 couleurs, t ∈ [0,1) boucle sur la palette
vec3 palette4(float t, vec4 cA, vec4 cB, vec4 cC, vec4 cD, int count, int mode) {
    t = fract(t);
    float n = clamp(float(count), 2.0, 4.0);
    float x = t * n;
    int idx = int(floor(x));
    float f = fract(x);
    vec3 c0, c1;
    if (idx == 0)      { c0 = cA.rgb; c1 = cB.rgb; }
    else if (idx == 1) { c0 = cB.rgb; c1 = (count > 2) ? cC.rgb : cA.rgb; }
    else if (idx == 2) { c0 = cC.rgb; c1 = (count > 3) ? cD.rgb : cA.rgb; }
    else               { c0 = cD.rgb; c1 = cA.rgb; }
    return palMix(c0, c1, f, mode);
}

// palette OUVERTE (dégradé A→…→D sans rebouclage) — pour ciels, hauteurs…
vec3 palette4Open(float t, vec4 cA, vec4 cB, vec4 cC, vec4 cD, int count, int mode) {
    t = clamp(t, 0.0, 1.0);
    float n = clamp(float(count), 2.0, 4.0) - 1.0;
    float x = min(t * n, n - 0.001);
    int idx = int(floor(x));
    float f = fract(x);
    vec3 c0, c1;
    if (idx == 0)      { c0 = cA.rgb; c1 = cB.rgb; }
    else if (idx == 1) { c0 = cB.rgb; c1 = cC.rgb; }
    else               { c0 = cC.rgb; c1 = cD.rgb; }
    return palMix(c0, c1, f, mode);
}

// ---------- easing / enveloppes -----------------------------------

float easeInOut(float t) { t = clamp(t, 0.0, 1.0); return t * t * (3.0 - 2.0 * t); }

// enveloppe attaque/décay sur une phase [0,1] (strobes stateless)
float envAD(float ph, float attack, float decay) {
    attack = max(attack, 1e-3);
    float a = clamp(ph / attack, 0.0, 1.0);
    float d = exp(-max(ph - attack, 0.0) / max(decay, 1e-3));
    return a * d;
}

// ---------- sortie alpha propre -----------------------------------
// Sortie PREMULTIPLIED : rgb = lumière émise (peut dépasser alpha,
// rendu additif), alpha borné [0,1]. Évite la classe de bugs
// « fond gris » et compose proprement plusieurs couches dans Arena.
vec4 domeOutput(vec3 col, float alpha, float mask) {
    return vec4(col * mask, clamp(alpha, 0.0, 1.0) * mask);
}

// ================================================================
// Dome Calibration Grid — utilitaire de calage (produit d'appel)
// Grille az/el + cardinaux + spring-line (tilt) + safe-area IMERSA
// + croix zénith. Convention : bas = avant, E à droite de l'image.
// ================================================================

// lettres N/E/S/W en segments, dans une boîte locale [0,1]²
// (x vers la droite du spectateur, y vers le zénith)
float letterSd(int idx, vec2 p) {
    float d = 1e3;
    if (idx == 0) {            // N
        d = min(d, sdSegment2(p, vec2(0.0, 0.0), vec2(0.0, 1.0)));
        d = min(d, sdSegment2(p, vec2(0.0, 1.0), vec2(1.0, 0.0)));
        d = min(d, sdSegment2(p, vec2(1.0, 0.0), vec2(1.0, 1.0)));
    } else if (idx == 1) {     // E
        d = min(d, sdSegment2(p, vec2(0.0, 0.0), vec2(0.0, 1.0)));
        d = min(d, sdSegment2(p, vec2(0.0, 1.0), vec2(0.9, 1.0)));
        d = min(d, sdSegment2(p, vec2(0.0, 0.5), vec2(0.7, 0.5)));
        d = min(d, sdSegment2(p, vec2(0.0, 0.0), vec2(0.9, 0.0)));
    } else if (idx == 2) {     // S
        d = min(d, sdSegment2(p, vec2(0.9, 1.0), vec2(0.0, 1.0)));
        d = min(d, sdSegment2(p, vec2(0.0, 1.0), vec2(0.0, 0.5)));
        d = min(d, sdSegment2(p, vec2(0.0, 0.5), vec2(0.9, 0.5)));
        d = min(d, sdSegment2(p, vec2(0.9, 0.5), vec2(0.9, 0.0)));
        d = min(d, sdSegment2(p, vec2(0.9, 0.0), vec2(0.0, 0.0)));
    } else {                   // W
        d = min(d, sdSegment2(p, vec2(0.0, 1.0), vec2(0.25, 0.0)));
        d = min(d, sdSegment2(p, vec2(0.25, 0.0), vec2(0.5, 0.7)));
        d = min(d, sdSegment2(p, vec2(0.5, 0.7), vec2(0.75, 0.0)));
        d = min(d, sdSegment2(p, vec2(0.75, 0.0), vec2(1.0, 1.0)));
    }
    return d;
}

void main() {
    // coordonnées indépendantes de la résolution, cercle centré
    vec2 p = (gl_FragCoord.xy - 0.5 * RENDERSIZE.xy) / (0.5 * min(RENDERSIZE.x, RENDERSIZE.y));
    vec2 uv = p * 0.5 + 0.5;
    float aa = 2.0 / min(RENDERSIZE.x, RENDERSIZE.y);

    float yaw   = radians(orientation);
    float tiltR = radians(domeTilt);

    vec2 azel = uvToDome(uv);
    azel = applyOrient(azel, orientation);
    float az = azel.x;
    float el = azel.y;
    float r  = length(p);
    float w  = lineWidth;

    // --- grille azimut (rayons) ---
    // pas arrondi au diviseur de 360° le plus proche : espacement uniforme,
    // y compris à la couture ±180° (outil de calibration oblige)
    float stepA = radians(max(gridStepAz, 2.0));
    stepA = TAU / max(floor(TAU / stepA + 0.5), 4.0);
    float azN = floor(az / stepA + 0.5) * stepA;
    float spokeFade = 1.0 - smoothstep(radians(78.0), radians(86.0), el);
    float spokes = strokeMask(sdMeridian(azel, azN), w, aa) * spokeFade;

    // rayons majeurs tous les 90° (axes cardinaux)
    float az90 = floor(az / HALF_PI + 0.5) * HALF_PI;
    float spokesMaj = strokeMask(sdMeridian(azel, az90), w * 1.8, aa) * spokeFade;

    // --- grille élévation (cercles) ---
    float stepE = radians(max(gridStepEl, 2.0));
    float elN = floor(el / stepE + 0.5) * stepE;
    float circles = 0.0;
    if (elN > 0.001 && elN < radians(81.0))
        circles = strokeMask(sdParallel(azel, elN), w, aa);
    float circMaj = max(strokeMask(sdParallel(azel, radians(30.0)), w * 1.8, aa),
                        strokeMask(sdParallel(azel, radians(60.0)), w * 1.8, aa));

    // bord du dôme (légèrement en retrait pour survivre au feather)
    float rimR = 1.0 - w * 3.0 - maskFeather;
    float rim = strokeMask(abs(r - rimR), w * 1.5, aa);

    // --- spring-line / horizon vrai (avec tilt) ---
    float spring = 0.0;
    if (showSpringLine && domeTilt > 0.05) {
        float dSpring = abs(el - springLineEl(az, tiltR)) / HALF_PI;
        spring = strokeMask(dSpring, w * 2.0, aa);
    }

    // --- safe-area IMERSA : az ±50°, el 10..60° ---
    float safe = 0.0;
    if (showSafeArea) {
        float a50 = radians(50.0), e10 = radians(10.0), e60 = radians(60.0);
        float dS = min(min(sdMeridianSeg(azel, -a50, e10, e60),
                           sdMeridianSeg(azel,  a50, e10, e60)),
                       min(sdParallelSeg(azel, e10, -a50, a50),
                           sdParallelSeg(azel, e60, -a50, a50)));
        safe = strokeMask(dS, w * 1.5, aa);
    }

    // --- croix zénith + cercle 80° ---
    float zen = 0.0;
    if (showZenith) {
        vec2 pr = rot2(-yaw) * p;
        float L = 0.06;
        float dz = min(sdSegment2(pr, vec2(-L, 0.0), vec2(L, 0.0)),
                       sdSegment2(pr, vec2(0.0, -L), vec2(0.0, L)));
        zen = strokeMask(dz, w * 1.5, aa);
        zen = max(zen, strokeMask(sdParallel(azel, radians(80.0)), w, aa));
    }

    // --- cardinaux N/E/S/W (N = avant/bas, E = droite image) ---
    float card = 0.0;
    if (showCardinals) {
        float letterH = 0.09;
        for (int i = 0; i < 4; i++) {
            float azC = float(i) * HALF_PI;
            float azS = azC + yaw;                       // azimut écran
            float elC = max(springLineEl(azC, tiltR), 0.0) + radians(7.0);
            vec2 pos = domeToScreen(azS, elC);
            vec2 rightV = vec2(cos(azS), sin(azS));      // sens des azimuts croissants
            vec2 upV    = vec2(-sin(azS), cos(azS));     // vers le zénith
            vec2 q = p - pos;
            vec2 pl = vec2(dot(q, rightV), dot(q, upV)) / letterH + vec2(0.5, 0.0);
            // évalue la lettre seulement près de sa boîte
            if (pl.x > -0.5 && pl.x < 1.5 && pl.y > -0.5 && pl.y < 1.5) {
                float dL = letterSd(i, pl) * letterH;
                card = max(card, strokeMask(dL, w * 1.6, aa));
            }
        }
    }

    // --- composition ---
    float base = max(max(spokes * 0.55, circles * 0.55),
                     max(spokesMaj * 0.85, circMaj * 0.85));
    base = max(base, max(zen, card));
    float accent = max(max(rim, spring), safe);

    vec3 col = lineColor.rgb * base * (1.0 - accent) + accentColor.rgb * accent;
    float alpha = max(base, accent);

    col *= intensity;
    alpha *= clamp(intensity, 0.0, 1.0);

    float mask = domeMask(uv, maskFeather);
    gl_FragColor = domeOutput(col, alpha, mask);
}
