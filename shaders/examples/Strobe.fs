/*{
  "DESCRIPTION": "Strobe couleur — fréquence et rapport cyclique pilotables (exemple Conduite)",
  "CREDIT": "Conduite (exemple)",
  "ISFVSN": "2",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "color", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "rate", "TYPE": "float", "MIN": 0.5, "MAX": 25.0, "DEFAULT": 8.0 },
    { "NAME": "duty", "TYPE": "float", "MIN": 0.05, "MAX": 0.95, "DEFAULT": 0.2 },
    { "NAME": "enabled", "TYPE": "bool", "DEFAULT": true }
  ]
}*/

void main() {
  float on = step(fract(TIME * rate), duty);
  float e = enabled ? 1.0 : 0.0;
  gl_FragColor = vec4(color.rgb * on * e, 1.0);
}
