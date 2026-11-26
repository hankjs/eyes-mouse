#import <AppKit/AppKit.h>
#include <stdbool.h>
#include <stdint.h>

typedef void (*ScreenVisitor)(void *context, uint32_t id, const char *name);

static uint32_t screen_id(NSScreen *screen) {
    return [screen.deviceDescription[@"NSScreenNumber"] unsignedIntValue];
}

static NSRect visible_frame(NSRect frame, NSRect visible) {
    frame.size.width = fmin(frame.size.width, visible.size.width);
    frame.size.height = fmin(frame.size.height, visible.size.height);
    frame.origin.x = fmax(NSMinX(visible), fmin(NSMinX(frame), NSMaxX(visible) - NSWidth(frame)));
    frame.origin.y = fmax(NSMinY(visible), fmin(NSMinY(frame), NSMaxY(visible) - NSHeight(frame)));
    return frame;
}

static NSRect centered_frame(NSRect frame, NSRect visible) {
    frame.origin.x = NSMidX(visible) - NSWidth(frame) / 2;
    frame.origin.y = NSMidY(visible) - NSHeight(frame) / 2;
    return visible_frame(frame, visible);
}

void eyes_mouse_visit_screens(ScreenVisitor visit, void *context) {
    for (NSScreen *screen in NSScreen.screens) {
        visit(context, screen_id(screen), screen.localizedName.UTF8String);
    }
}

bool eyes_mouse_place_exercise(void *handle, uint32_t id, uint32_t *original_id, double original[4]) {
    NSWindow *window = (__bridge NSWindow *)handle;
    NSRect frame = window.frame;
    NSScreen *source = window.screen;
    *original_id = screen_id(source);
    original[0] = source ? NSMinX(frame) - NSMinX(source.frame) : 0;
    original[1] = source ? NSMinY(frame) - NSMinY(source.frame) : 0;
    original[2] = NSWidth(frame);
    original[3] = NSHeight(frame);
    if (*original_id == id) return true;

    for (NSScreen *screen in NSScreen.screens) {
        if (screen_id(screen) != id) continue;
        [window setFrame:centered_frame(frame, screen.visibleFrame) display:YES];
        return screen_id(window.screen) == id;
    }
    return false;
}

void eyes_mouse_restore_exercise(void *handle, uint32_t original_id, const double original[4]) {
    NSWindow *window = (__bridge NSWindow *)handle;
    NSRect frame = NSMakeRect(original[0], original[1], original[2], original[3]);
    NSArray<NSScreen *> *screens = NSScreen.screens;
    for (NSScreen *screen in screens) {
        if (original_id != 0 && screen_id(screen) == original_id) {
            // Changing the primary display changes AppKit's global origin.
            NSRect restored = NSOffsetRect(frame, NSMinX(screen.frame), NSMinY(screen.frame));
            [window setFrame:visible_frame(restored, screen.visibleFrame) display:YES];
            return;
        }
    }
    if (screens.firstObject) {
        [window setFrame:centered_frame(frame, screens.firstObject.visibleFrame) display:YES];
    }
}

bool eyes_mouse_exercise_target(void *handle, uint32_t id, double x, double y, double target[2]) {
    NSWindow *window = (__bridge NSWindow *)handle;
    NSScreen *screen = window.screen;
    NSView *view = window.contentView;
    if (!view || screen_id(screen) != id || !NSEqualRects(window.frame, screen.frame)) return false;

    // AppKit provides both rectangles in logical points. Convert to normalized top-down coordinates.
    NSRect content = [window convertRectToScreen:[view convertRect:view.bounds toView:nil]];
    NSRect frame = screen.frame;
    target[0] = (NSMinX(content) - NSMinX(frame) + x * NSWidth(content)) / NSWidth(frame);
    target[1] = (NSMaxY(frame) - NSMaxY(content) + y * NSHeight(content)) / NSHeight(frame);
    return true;
}
