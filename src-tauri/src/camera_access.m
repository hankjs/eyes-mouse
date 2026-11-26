#import <AVFoundation/AVFoundation.h>
#include <stdbool.h>

void eyes_mouse_request_camera_access(void (*reply)(bool, void *), void *context) {
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeVideo completionHandler:^(BOOL granted) {
        reply(granted, context);
    }];
}

bool eyes_mouse_only_camera_is_builtin(void) {
    @autoreleasepool {
        // The pinned OpenCV AVFoundation backend selects index 0 from this list.
        // With exactly one device, its index is unambiguous.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        NSArray<AVCaptureDevice *> *video = [AVCaptureDevice devicesWithMediaType:AVMediaTypeVideo];
        NSArray<AVCaptureDevice *> *muxed = [AVCaptureDevice devicesWithMediaType:AVMediaTypeMuxed];
#pragma clang diagnostic pop
        AVCaptureDevice *device = video.firstObject;
        if (@available(macOS 13.0, *)) {
            return video.count == 1 && muxed.count == 0 && !device.isContinuityCamera &&
                [device.deviceType isEqualToString:AVCaptureDeviceTypeBuiltInWideAngleCamera];
        }
        return false;
    }
}
