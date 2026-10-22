#version 100

// Background post-processing after the blur, one pass from the blurred texture into a new one:
// hyprlock's vibrancy (its FRAGBLUR1, renderer/Shaders.hpp, BSD-3-Clause), then swayward's blur
// saturation and noise (its render_helpers/shaders/postprocess.frag). See COPIED.md.

precision highp float;

varying vec2 v_coords;

uniform sampler2D tex;
uniform int passes;
uniform float vibrancy;
uniform float vibrancy_darkness;
uniform float saturation;
uniform float noise;

// hyprlock FRAGBLUR1. See http://alienryderflex.com/hsp.html
const float Pr = 0.299;
const float Pg = 0.587;
const float Pb = 0.114;

// Y is "v" (brightness). X is "s" (saturation); see https://www.desmos.com/3d/a88652b9a4
// Determines if high brightness or high saturation is more important.
const float a = 0.93;
const float b = 0.11;
const float c = 0.66; // The smoothness of the transition of unboosted to boosted colours.

// http://www.flong.com/archive/texts/code/shapers_circ/
float doubleCircleSigmoid(float x, float a) {
    a = clamp(a, 0.0, 1.0);

    float y = .0;
    if (x <= a) {
        y = a - sqrt(a * a - x * x);
    } else {
        y = a + sqrt(pow(1. - a, 2.) - pow(x - 1., 2.));
    }
    return y;
}

vec3 rgb2hsl(vec3 col) {
    float red   = col.r;
    float green = col.g;
    float blue  = col.b;

    float minc  = min(col.r, min(col.g, col.b));
    float maxc  = max(col.r, max(col.g, col.b));
    float delta = maxc - minc;

    float lum = (minc + maxc) * 0.5;
    float sat = 0.0;
    float hue = 0.0;

    if (lum > 0.0 && lum < 1.0) {
        float mul = (lum < 0.5) ? (lum) : (1.0 - lum);
        sat       = delta / (mul * 2.0);
    }

    if (delta > 0.0) {
        vec3  maxcVec = vec3(maxc);
        vec3  masks = vec3(equal(maxcVec, col)) * vec3(notEqual(maxcVec, vec3(green, blue, red)));
        vec3  adds = vec3(0.0, 2.0, 4.0) + vec3(green - blue, blue - red, red - green) / delta;

        hue += dot(adds, masks);
        hue /= 6.0;

        if (hue < 0.0)
            hue += 1.0;
    }

    return vec3(hue, sat, lum);
}

vec3 hsl2rgb(vec3 col) {
    const float onethird = 1.0 / 3.0;
    const float twothird = 2.0 / 3.0;
    const float rcpsixth = 6.0;

    float       hue = col.x;
    float       sat = col.y;
    float       lum = col.z;

    vec3        xt = vec3(0.0);

    if (hue < onethird) {
        xt.r = rcpsixth * (onethird - hue);
        xt.g = rcpsixth * hue;
        xt.b = 0.0;
    } else if (hue < twothird) {
        xt.r = 0.0;
        xt.g = rcpsixth * (twothird - hue);
        xt.b = rcpsixth * (hue - onethird);
    } else
        xt = vec3(rcpsixth * (hue - twothird), 0.0, rcpsixth * (1.0 - hue));

    xt = min(xt, 1.0);

    float sat2   = 2.0 * sat;
    float satinv = 1.0 - sat;
    float luminv = 1.0 - lum;
    float lum2m1 = (2.0 * lum) - 1.0;
    vec3  ct     = (sat2 * xt) + satinv;

    vec3  rgb;
    if (lum >= 0.5)
        rgb = (luminv * ct) + lum2m1;
    else
        rgb = lum * ct;

    return rgb;
}

// The body of FRAGBLUR1's vibrancy branch, on one colour, with vibrancy already divided by the
// pass count.
vec3 vibrancyBoost(vec3 color, float vib) {
    // Invert it so that it correctly maps to the config setting.
    float vibrancy_darkness1 = 1.0 - vibrancy_darkness;

    vec3 hsl = rgb2hsl(color);
    // Perceived brightness, so visually dark colours like deep blue are boosted less than equally
    // saturated yellow.
    float perceivedBrightness = doubleCircleSigmoid(sqrt(color.r * color.r * Pr + color.g * color.g * Pg + color.b * color.b * Pb), 0.8 * vibrancy_darkness1);

    float b1        = b * vibrancy_darkness1;
    float boostBase = hsl[1] > 0.0 ? smoothstep(b1 - c * 0.5, b1 + c * 0.5, 1.0 - (pow(1.0 - hsl[1] * cos(a), 2.0) + pow(1.0 - perceivedBrightness * sin(a), 2.0))) : 0.0;

    float sat = clamp(hsl[1] + boostBase * vib, 0.0, 1.0);

    return hsl2rgb(vec3(hsl[0], sat, hsl[2]));
}

// swayward postprocess.frag.
// Sin-less white noise by David Hoskins (MIT License).
// https://www.shadertoy.com/view/4djSRW
float hash12(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

vec3 saturate(vec3 color, float sat) {
    const vec3 w = vec3(0.2126, 0.7152, 0.0722);
    return mix(vec3(dot(color, w)), color, sat);
}

void main() {
    vec4 color = texture2D(tex, v_coords);
    if (color.a <= 0.0) {
        gl_FragColor = vec4(0.0);
        return;
    }
    // The texture is premultiplied; the colour maths (vibrancy is not linear) runs on the colour.
    vec3 rgb = color.rgb / color.a;

    // hyprlock boosts once per blur down pass at vibrancy / passes. The blur is done here, so the
    // passes run back to back: the same result for a uniform colour, close for the rest.
    if (vibrancy > 0.0) {
        float vib = vibrancy / float(passes);
        for (int i = 0; i < 31; i++) {
            if (i >= passes)
                break;
            rgb = vibrancyBoost(rgb, vib);
        }
    }

    if (saturation != 1.0) {
        rgb = saturate(rgb, saturation);
    }

    if (noise > 0.0) {
        rgb += (hash12(gl_FragCoord.xy) - 0.5) * noise;
    }

    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0) * color.a, color.a);
}
