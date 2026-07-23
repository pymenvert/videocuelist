/*{
  "DESCRIPTION": "Plasma classique — échelle, vitesse et teinte pilotables (exemple Conduite)",
  "CREDIT": "Conduite (exemple)",
  "ISFVSN": "2",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "scale", "TYPE": "float", "MIN": 1.0, "MAX": 20.0, "DEFAULT": 6.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 0.6 },
    { "NAME": "hueShift", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "contrast", "TYPE": "float", "MIN": 0.2, "MAX": 3.0, "DEFAULT": 1.0 }
  ]
}*/

vec3 hsv2rgb(vec3 c) {
  vec3 p = abs(fract(c.xxx + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
  return c.z * mix(vec3(1.0), clamp(p - 1.0, 0.0, 1.0), c.y);
}

void main() {
  vec2 uv = isf_FragNormCoord * scale;
  float t = TIME * speed;
  float v = sin(uv.x + t) + sin(uv.y + t * 0.7)
          + sin((uv.x + uv.y) * 0.7 + t * 1.3)
          + sin(length(uv - scale * 0.5) + t);
  v = v * 0.25 * contrast;
  vec3 rgb = hsv2rgb(vec3(fract(v * 0.5 + hueShift), 0.8, clamp(0.5 + 0.5 * sin(v * 3.14159), 0.0, 1.0)));
  gl_FragColor = vec4(rgb, 1.0);
}
