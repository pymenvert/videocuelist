/*{
  "DESCRIPTION": "Dégradé animé deux couleurs — exemple Conduite",
  "CREDIT": "Conduite (exemple)",
  "ISFVSN": "2",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.05, 0.15, 0.5, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [0.9, 0.3, 0.1, 1.0] },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.5 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 6.2832, "DEFAULT": 0.0 }
  ]
}*/

void main() {
  vec2 uv = isf_FragNormCoord;
  vec2 dir = vec2(cos(angle), sin(angle));
  float t = dot(uv - 0.5, dir) + 0.5;
  t = fract(t + TIME * speed * 0.25);
  float w = smoothstep(0.0, 0.5, t) * (1.0 - smoothstep(0.5, 1.0, t)) * 2.0;
  gl_FragColor = mix(colorA, colorB, w);
}
