#import <AVFoundation/AVFoundation.h>
#include <stdbool.h>

void eyes_mouse_request_camera_access(void (*reply)(bool, void *), void *context) {
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeVideo completionHandler:^(BOOL granted) {
        reply(granted, context);
    }];
}
