#import <AppKit/AppKit.h>
#include <stdbool.h>

void eyes_mouse_show_snake(void *handle) {
    NSWindow *window = (__bridge NSWindow *)handle;
    // Keep the same window and webview when the active Space changes.
    window.collectionBehavior |= NSWindowCollectionBehaviorCanJoinAllSpaces
        | NSWindowCollectionBehaviorFullScreenAuxiliary
        | NSWindowCollectionBehaviorCanJoinAllApplications;
    window.ignoresMouseEvents = YES;
    window.hidesOnDeactivate = NO;
    window.excludedFromWindowsMenu = YES;
    [window orderFrontRegardless];
}

bool eyes_mouse_snake_pointer(void *handle, double *x, double *y) {
    NSWindow *window = (__bridge NSWindow *)handle;
    NSView *content = window.contentView;
    if (!content) return false;

    // Both AppKit coordinates and the webview layout use logical points.
    NSRect bounds = content.bounds;
    NSPoint point = [content convertPoint:window.mouseLocationOutsideOfEventStream fromView:nil];
    *x = point.x - NSMinX(bounds);
    *y = content.isFlipped ? point.y - NSMinY(bounds) : NSMaxY(bounds) - point.y;
    return *x >= 0 && *x < NSWidth(bounds) && *y >= 0 && *y < NSHeight(bounds);
}
