// Super Media Player: contrast adaptive sharpening (after AMD's CAS idea).
// Sharpens edges in proportion to how much room there is before they would
// clip, so already-crisp edges and flat areas are left alone.
// GPL-3.0-or-later, like the rest of the player.

//!HOOK LUMA
//!BIND HOOKED
//!DESC SMP sharpen (CAS)

#define SHARPNESS 0.6

vec4 hook() {
    float b = HOOKED_texOff(vec2(0.0, -1.0)).x;
    float d = HOOKED_texOff(vec2(-1.0, 0.0)).x;
    float e = HOOKED_tex(HOOKED_pos).x;
    float f = HOOKED_texOff(vec2(1.0, 0.0)).x;
    float h = HOOKED_texOff(vec2(0.0, 1.0)).x;
    float mn = min(min(min(d, e), min(f, b)), h);
    float mx = max(max(max(d, e), max(f, b)), h);
    float amp = sqrt(clamp(min(mn, 1.0 - mx) / max(mx, 1e-5), 0.0, 1.0));
    float w = amp * (-1.0 / mix(8.0, 5.0, SHARPNESS));
    float o = (e + (b + d + f + h) * w) / (1.0 + 4.0 * w);
    return vec4(clamp(o, 0.0, 1.0), 0.0, 0.0, 1.0);
}
