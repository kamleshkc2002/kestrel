/*
 * Disposable X11 XGrabKey probe for Kestrel Issue #7.
 *
 * Build: gcc -Wall -Wextra -O2 -o xgrabkey_probe xgrabkey_probe.c -lX11
 * Run:   ./xgrabkey_probe
 *
 * Validates only the X11 global-shortcut fallback surface:
 *   - XOpenDisplay reachability through Xwayland (DISPLAY provided by session)
 *   - passive grab registration (XGrabKey) on the root window and on a
 *     fresh application window
 *   - duplicate/conflicting grab behavior across two independent X client
 *     connections
 *   - ungrab cleanup and successful re-grab when a free combination exists
 *
 * The probe never injects keys, never waits for events, and never modifies
 * user configuration.
 */
#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <stdio.h>

static const char *grab_status_name(int rc) {
    switch (rc) {
        case GrabSuccess: return "GrabSuccess";
        case AlreadyGrabbed: return "AlreadyGrabbed";
        case GrabNotViewable: return "GrabNotViewable";
        case GrabFrozen: return "GrabFrozen";
        case GrabInvalidTime: return "GrabInvalidTime";
        default: return "Other";
    }
}

int main(void) {
    Display *a = XOpenDisplay(NULL);
    if (a == NULL) {
        printf("RESULT xopen_display=FAILED\n");
        return 2;
    }

    Display *b = XOpenDisplay(NULL); /* second connection for conflict test */
    int screen = DefaultScreen(a);
    Window root = RootWindow(a, screen);
    Window child = XCreateSimpleWindow(a, root, 0, 0, 1, 1, 0, 0, 0);
    XMapWindow(a, child);
    XSync(a, False);

    printf("display=%s\n", DisplayString(a));

    static const KeySym keysyms[] = { XK_F12, XK_a, XK_space };
    static const char *keysym_names[] = { "F12", "a", "space" };
    static const struct { unsigned int mask; const char *label; } mods[] = {
        { 0,                  "none"     },
        { ControlMask | Mod1Mask, "Ctrl+Alt" },
    };

    int found_success = 0;
    KeyCode chosen_kc = 0;
    unsigned int chosen_mask = 0;

    for (size_t i = 0; i < sizeof(keysyms) / sizeof(keysyms[0]); i++) {
        KeyCode kc = XKeysymToKeycode(a, keysyms[i]);
        if (kc == 0) {
            printf("scan %s keycode=unmapped\n", keysym_names[i]);
            continue;
        }
        for (size_t j = 0; j < sizeof(mods) / sizeof(mods[0]); j++) {
            int rr = XGrabKey(a, kc, mods[j].mask, root, True,
                              GrabModeAsync, GrabModeAsync);
            int rw = XGrabKey(a, kc, mods[j].mask, child, True,
                              GrabModeAsync, GrabModeAsync);
            printf("scan %s keycode=%u modifiers=0x%x (%s) root=%s child=%s\n",
                   keysym_names[i], (unsigned int)kc, mods[j].mask, mods[j].label,
                   grab_status_name(rr), grab_status_name(rw));
            if (rr == GrabSuccess) XUngrabKey(a, kc, mods[j].mask, root);
            if (rw == GrabSuccess) XUngrabKey(a, kc, mods[j].mask, child);
            if (!found_success && (rr == GrabSuccess || rw == GrabSuccess)) {
                found_success = 1;
                chosen_kc = kc;
                chosen_mask = mods[j].mask;
            }
        }
    }

    if (found_success) {
        /* Conflict detection: second independent client, same combination. */
        int g1 = XGrabKey(a, chosen_kc, chosen_mask, root, True,
                          GrabModeAsync, GrabModeAsync);
        printf("chosen keycode=%u modifiers=0x%x conn_a=%s (%d)\n",
               (unsigned int)chosen_kc, chosen_mask, grab_status_name(g1), g1);
        if (b != NULL) {
            int g2 = XGrabKey(b, chosen_kc, chosen_mask, root, True,
                              GrabModeAsync, GrabModeAsync);
            printf("conflict conn_b_same_combo=%s (%d)\n",
                   grab_status_name(g2), g2);
        }
        /* Revocation / cleanup: ungrab, then confirm a fresh grab succeeds. */
        XUngrabKey(a, chosen_kc, chosen_mask, root);
        int g3 = XGrabKey(a, chosen_kc, chosen_mask, root, True,
                          GrabModeAsync, GrabModeAsync);
        printf("ungrab_then_regrab_conn_a=%s (%d)\n", grab_status_name(g3), g3);
        XUngrabKey(a, chosen_kc, chosen_mask, root);
    } else {
        printf("RESULT no_free_combo_found (XGrabKey surface is occupied)\n");
    }

    XSync(a, False);
    XDestroyWindow(a, child);
    if (b != NULL) {
        XSync(b, False);
        XCloseDisplay(b);
    }
    XCloseDisplay(a);
    return 0;
}

