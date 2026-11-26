#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#include <stdbool.h>

void eyes_mouse_prompt_accessibility(void) {
    @autoreleasepool {
        NSDictionary *options = @{(__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES};
        AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)options);
    }
}

bool eyes_mouse_open_accessibility_settings(void) {
    @autoreleasepool {
        NSURL *url = [NSURL URLWithString:@"x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility"];
        return [NSWorkspace.sharedWorkspace openURL:url];
    }
}
