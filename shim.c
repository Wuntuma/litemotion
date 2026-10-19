/* Minimal playback-only wrapper around miniaudio. The Rust side owns all
   buffering; this just calls back to fill f32 stereo frames. */
#define MA_NO_DECODING
#define MA_NO_ENCODING
#define MA_NO_GENERATION
#define MA_NO_RESOURCE_MANAGER
#define MA_NO_NODE_GRAPH
#define MINIAUDIO_IMPLEMENTATION
#include "miniaudio.h"
#include <stdlib.h>

typedef void (*lm_audio_cb)(void *user, float *out, unsigned int frames);
typedef struct {
    ma_context ctx;
    int has_ctx;
    ma_device dev;
    lm_audio_cb cb;
    void *user;
} lm_audio;

static void lm_data(ma_device *d, void *out, const void *in, ma_uint32 n) {
    lm_audio *a = (lm_audio *)d->pUserData;
    (void)in;
    a->cb(a->user, (float *)out, n);
}

lm_audio *lm_audio_open(lm_audio_cb cb, void *user, int null_backend, unsigned int rate) {
    lm_audio *a = (lm_audio *)calloc(1, sizeof(lm_audio));
    ma_device_config c;
    ma_context *ctx = NULL;
    if (!a) return NULL;
    a->cb = cb;
    a->user = user;
    c = ma_device_config_init(ma_device_type_playback);
    c.playback.format = ma_format_f32;
    c.playback.channels = 2;
    c.sampleRate = rate;
    c.dataCallback = lm_data;
    c.pUserData = a;
    if (null_backend) {
        ma_backend b[1];
        b[0] = ma_backend_null;
        if (ma_context_init(b, 1, NULL, &a->ctx) != MA_SUCCESS) { free(a); return NULL; }
        a->has_ctx = 1;
        ctx = &a->ctx;
    }
    if (ma_device_init(ctx, &c, &a->dev) != MA_SUCCESS) {
        if (a->has_ctx) ma_context_uninit(&a->ctx);
        free(a);
        return NULL;
    }
    return a;
}
int lm_audio_start(lm_audio *a) { return ma_device_start(&a->dev) == MA_SUCCESS; }
void lm_audio_stop(lm_audio *a) { ma_device_stop(&a->dev); }
void lm_audio_close(lm_audio *a) {
    ma_device_uninit(&a->dev);
    if (a->has_ctx) ma_context_uninit(&a->ctx);
    free(a);
}
