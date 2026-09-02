// Apple Vision text recognition, behind a C ABI.
//
// One entry point, one direction: pixels in, JSON out. The JSON is a detail of
// this boundary rather than a wire format — it exists because a text region is
// a string and a box, and returning a variable number of those across FFI by
// hand is more machinery than parsing one document is worth.
//
// Everything Vision-shaped stops here. `vision.rs` knows about regions and
// quads; it does not know about CGImage, and nothing above it knows this file
// exists.

#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <Vision/Vision.h>

static char *wilkes_strdup(NSString *s) {
    const char *utf8 = [s UTF8String];
    if (utf8 == NULL) {
        return NULL;
    }
    size_t n = strlen(utf8) + 1;
    char *out = malloc(n);
    if (out != NULL) {
        memcpy(out, utf8, n);
    }
    return out;
}

static void wilkes_fail(char **error_out, NSString *message) {
    if (error_out != NULL) {
        *error_out = wilkes_strdup(message);
    }
}

/// Free a string returned by this module. Every non-NULL `char *` handed out
/// here — result or error — comes back through this.
void wilkes_vision_string_free(char *s) {
    if (s != NULL) {
        free(s);
    }
}

/// Recognize text in one packed RGB8 image.
///
/// Returns malloc'd UTF-8 JSON on success, or NULL with `*error_out` set to a
/// malloc'd message. Never both, never neither.
///
/// The JSON is `{"regions":[{"text":..,"confidence":..,"x":..,"y":..,"w":..,"h":..}]}`
/// where the box is Vision's own normalised rect: origin bottom-left, which is
/// not Wilkes' convention. The flip belongs to the side that knows what a quad
/// is, so it happens in `vision.rs` and not here.
char *wilkes_vision_recognize_rgb(const uint8_t *rgb, size_t width, size_t height,
                                  char **error_out) {
    if (error_out != NULL) {
        *error_out = NULL;
    }
    if (rgb == NULL || width == 0 || height == 0) {
        wilkes_fail(error_out, @"an empty image cannot be recognized");
        return NULL;
    }

    @autoreleasepool {
        // CGBitmapContext has no 24-bit RGB layout, so the buffer is widened to
        // RGBX once here rather than being carried as RGBA through the Rust
        // side, where every other engine wants three channels.
        size_t count = width * height;
        if (count > SIZE_MAX / 4) {
            wilkes_fail(error_out, @"image too large to widen to RGBX");
            return NULL;
        }
        uint8_t *rgbx = malloc(count * 4);
        if (rgbx == NULL) {
            wilkes_fail(error_out, @"out of memory widening the image to RGBX");
            return NULL;
        }
        for (size_t i = 0; i < count; i++) {
            rgbx[i * 4 + 0] = rgb[i * 3 + 0];
            rgbx[i * 4 + 1] = rgb[i * 3 + 1];
            rgbx[i * 4 + 2] = rgb[i * 3 + 2];
            rgbx[i * 4 + 3] = 255;
        }

        CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
        CGContextRef ctx =
            CGBitmapContextCreate(rgbx, width, height, 8, width * 4, space,
                                  (CGBitmapInfo)kCGImageAlphaNoneSkipLast);
        CGColorSpaceRelease(space);
        if (ctx == NULL) {
            free(rgbx);
            wilkes_fail(error_out, @"could not create a bitmap context for the image");
            return NULL;
        }
        CGImageRef image = CGBitmapContextCreateImage(ctx);
        CGContextRelease(ctx);
        if (image == NULL) {
            free(rgbx);
            wilkes_fail(error_out, @"could not create a CGImage for the image");
            return NULL;
        }

        VNRecognizeTextRequest *request = [[VNRecognizeTextRequest alloc] init];
        request.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
        request.usesLanguageCorrection = YES;

        VNImageRequestHandler *handler =
            [[VNImageRequestHandler alloc] initWithCGImage:image options:@{}];

        NSError *error = nil;
        BOOL ok = [handler performRequests:@[ request ] error:&error];
        CGImageRelease(image);
        free(rgbx);

        if (!ok) {
            wilkes_fail(error_out,
                        error ? [error localizedDescription] : @"Vision failed without an error");
            return NULL;
        }

        NSMutableArray *regions = [NSMutableArray array];
        for (VNRecognizedTextObservation *obs in request.results) {
            VNRecognizedText *best = [[obs topCandidates:1] firstObject];
            if (best == nil || best.string.length == 0) {
                continue;
            }
            CGRect box = obs.boundingBox;
            [regions addObject:@{
                @"text" : best.string,
                @"confidence" : @(best.confidence),
                @"x" : @(box.origin.x),
                @"y" : @(box.origin.y),
                @"w" : @(box.size.width),
                @"h" : @(box.size.height),
            }];
        }

        NSError *jsonError = nil;
        NSData *json = [NSJSONSerialization dataWithJSONObject:@{@"regions" : regions}
                                                       options:0
                                                         error:&jsonError];
        if (json == nil) {
            wilkes_fail(error_out, jsonError ? [jsonError localizedDescription]
                                             : @"could not serialize the recognition");
            return NULL;
        }
        NSString *text = [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
        char *out = wilkes_strdup(text);
        if (out == NULL) {
            wilkes_fail(error_out, @"out of memory copying the recognition out");
        }
        return out;
    }
}
