// CarterCanvas engine: the C ABI of the `cartercanvas-ios` Rust crate (src/ffi.rs).
//
// Every function runs on the main thread. `CCApp *` is the handle `cc_create` returns; pass it
// until `cc_destroy`. Strings are NUL-terminated UTF-8 and only need to live for the call.
// Coordinates are in points within the Metal view, origin at the top left.

#ifndef CARTERCANVAS_H
#define CARTERCANVAS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CcApp CCApp;

/// File dialog kinds (`file_dialog`'s `kind`).
enum {
    CC_DIALOG_OPEN_ONE = 0,
    CC_DIALOG_OPEN_MANY = 1,
    CC_DIALOG_FOLDER = 2,
    CC_DIALOG_SAVE = 3,
};

/// `cc_frame` result bits.
enum {
    CC_FRAME_DREW = 1,
    CC_FRAME_WANTS_KEYBOARD = 2,
};

/// Touch phases and kinds (`CCTouch`).
enum {
    CC_TOUCH_BEGAN = 0,
    CC_TOUCH_MOVED = 1,
    CC_TOUCH_ENDED = 2,
    CC_TOUCH_CANCELLED = 3,
};
enum {
    CC_TOUCH_FINGER = 0,
    CC_TOUCH_PENCIL = 1,
};

/// Haptic kinds (`haptic`).
enum {
    CC_HAPTIC_TICK = 0,
    CC_HAPTIC_SUCCESS = 1,
    CC_HAPTIC_WARNING = 2,
};

/// One touch or Apple Pencil sample.
typedef struct CCTouch {
    uint64_t id;          // stable for the touch's lifetime
    uint32_t phase;       // CC_TOUCH_BEGAN…
    uint32_t kind;        // CC_TOUCH_FINGER or CC_TOUCH_PENCIL
    float x, y;           // points
    float pressure;       // Pencil: force / maximumPossibleForce (0…1)
    float altitude;       // Pencil: radians, 0 flat … π/2 upright
    float azimuth;        // Pencil: radians in the view (0 = pointing right)
    float roll;           // Pencil Pro barrel roll, radians (0 if unknown)
    double timestamp;     // UITouch.timestamp
} CCTouch;

/// Services the host provides. Any entry may be NULL (the feature reports itself unavailable).
/// Each gets `user_data` first. Called on the main thread, from inside `cc_*` calls.
typedef struct CCHostCallbacks {
    void *user_data;
    /// Show a document picker. `json`: {"extensions":[…]|null,"initialDir":…} for open/folder,
    /// {"suggested":name,"folder":path,"documents":path} for save. Answer later with
    /// `cc_file_dialog_answer` (or `cc_file_dialog_failed`) and the same `request_id`.
    void (*file_dialog)(void *user_data, uint64_t request_id, uint32_t kind, const char *json);
    bool (*open_url)(void *user_data, const char *url);
    /// Put RGBA8 (unpremultiplied) pixels on the pasteboard.
    bool (*clipboard_set_image)(void *user_data, uint32_t width, uint32_t height, const uint8_t *rgba, size_t len);
    /// Read the pasteboard image: call `cc_image_sink_put(sink, …)` before returning if there is one.
    void (*clipboard_get_image)(void *user_data, void *sink);
    void (*clipboard_set_text)(void *user_data, const char *text);
    void (*set_keyboard_visible)(void *user_data, bool visible);
    void (*haptic)(void *user_data, uint32_t kind);
    void (*share_file)(void *user_data, const char *path);
    /// A text field wants the pasteboard text: answer with `cc_paste`.
    void (*request_paste)(void *user_data);
} CCHostCallbacks;

/// Start the engine on `metal_layer` (a CAMetalLayer, unretained; it must outlive the app).
/// Returns NULL on failure; the reason is in <config_dir>/Logs/cartercanvas.log.
CCApp *cc_create(void *metal_layer, uint32_t width_px, uint32_t height_px, float scale,
                 const char *config_dir, const char *documents_dir, const CCHostCallbacks *callbacks);
void cc_destroy(CCApp *app);

/// Size in pixels, scale, and safe-area insets in points.
void cc_resize(CCApp *app, uint32_t width_px, uint32_t height_px, float scale,
               float inset_top, float inset_left, float inset_bottom, float inset_right);
/// Call on every display-link tick with CACurrentMediaTime(). Returns CC_FRAME_* bits.
uint32_t cc_frame(CCApp *app, double timestamp);

void cc_touches(CCApp *app, const CCTouch *touches, size_t count);
void cc_hover(CCApp *app, float x, float y, bool active);
void cc_pointer_button(CCApp *app, float x, float y, uint32_t button, bool pressed);
void cc_scroll(CCApp *app, float dx, float dy);
void cc_pinch(CCApp *app, float x, float y, float factor);

void cc_modifiers(CCApp *app, uint32_t modifier_flags);
/// A hardware key (UIKeyboardHIDUsage). Returns whether the engine used it.
bool cc_key(CCApp *app, uint32_t hid_usage, bool pressed, uint32_t modifier_flags, bool repeat, const char *text);
void cc_text(CCApp *app, const char *text);
void cc_delete_backward(CCApp *app);
void cc_paste(CCApp *app, const char *text);

/// `paths_json`: a JSON array of paths, or NULL when the user cancelled.
void cc_file_dialog_answer(CCApp *app, uint64_t request_id, uint32_t kind, const char *paths_json);
void cc_file_dialog_failed(CCApp *app, uint64_t request_id, const char *message);
void cc_open_paths(CCApp *app, const char *paths_json);
void cc_image_sink_put(void *sink, uint32_t width, uint32_t height, const uint8_t *rgba, size_t len);

void cc_set_focused(CCApp *app, bool focused);
void cc_background(CCApp *app);
void cc_memory_warning(CCApp *app);
bool cc_has_unsaved(CCApp *app);

#ifdef __cplusplus
}
#endif

#endif
