/*
 OpenASIO: a small, permissive, ASIO-like realtime audio driver ABI for Linux and friends.
 SPDX-License-Identifier: MIT OR Apache-2.0

 NOTE: OpenASIO is NOT affiliated with Steinberg ASIO®; it's an open alternative.
 This header defines a C99 ABI separating a DAW host from a low-latency driver.
*/
#ifndef OPENASIO_H
#define OPENASIO_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>
#include <stddef.h>

#define OA_VERSION_MAJOR 0
#define OA_VERSION_MINOR 1
#define OA_VERSION_PATCH 0

#if defined(_WIN32) || defined(__CYGWIN__)
  #ifdef OA_BUILDING_DLL
    #define OA_API __declspec(dllexport)
  #else
    #define OA_API __declspec(dllimport)
  #endif
#else
  #define OA_API __attribute__((visibility("default")))
#endif

typedef int32_t oa_bool;
enum { OA_FALSE = 0, OA_TRUE = 1 };

typedef enum {
  OA_OK = 0,
  OA_ERR_GENERIC = -1,
  OA_ERR_UNSUPPORTED = -2,
  OA_ERR_INVALID_ARG = -3,
  OA_ERR_DEVICE = -4,
  OA_ERR_BACKEND = -5,
} oa_result;

typedef enum {
  OA_SAMPLE_F32 = 1,   // native float32 [-1, +1]
  OA_SAMPLE_I16 = 2,   // signed 16-bit
  OA_SAMPLE_U16 = 3,   // unsigned 16-bit
} oa_sample_format;

typedef struct {
  uint32_t sample_rate;     // Hz
  uint32_t buffer_frames;   // frames per callback (target; driver may adjust)
  uint16_t in_channels;     // number of input channels
  uint16_t out_channels;    // number of output channels
  oa_sample_format format;  // sample format
  oa_bool interleaved;      // OA_TRUE = interleaved
} oa_stream_config;

typedef struct {
  uint64_t host_time_ns;  // host monotonic time for this callback
  uint64_t device_time_ns;// optional device clock ns (0 if unknown)
  uint32_t underruns;     // since last callback
  uint32_t overruns;      // since last callback
} oa_time_info;

// Forward decl
struct oa_driver;
typedef struct oa_driver oa_driver;

// Host-provided callbacks (invoked by driver on the audio thread)
typedef struct {
  // Audio process: the driver calls this for each buffer.
  // in/out point to either interleaved (if interleaved=TRUE) or non-interleaved channel arrays.
  // The host must return OA_TRUE to continue, or OA_FALSE to request stop.
  oa_bool (*process)(void *user,
                     const void *in,  // const float* or const void** depending on format/layout
                     void *out,       // float* or void** depending on format/layout
                     uint32_t frames,
                     const oa_time_info *time,
                     const oa_stream_config *cfg);
  // Optional notifications (may be NULL)
  void (*latency_changed)(void *user, uint32_t input_latency, uint32_t output_latency);
  void (*reset_request)(void *user);
} oa_host_callbacks;

// Creation parameters for a driver
typedef struct {
  uint32_t struct_size;      // must be set to sizeof(oa_create_params)
  const oa_host_callbacks *host;
  void *host_user;           // forwarded to callbacks
} oa_create_params;

// Driver interface (function table)
typedef struct {
  uint32_t struct_size; // sizeof(oa_driver_vtable)

  oa_result (*query_devices)(oa_driver *self, char *buf, size_t buf_len);
  // Open a named device (NULL or "" = default); returns >=0 device_id or <0 error
  int32_t  (*open_device)(oa_driver *self, const char *name);
  oa_result (*close_device)(oa_driver *self);

  oa_result (*get_default_config)(oa_driver *self, oa_stream_config *out);
  oa_result (*start)(oa_driver *self, const oa_stream_config *cfg);
  oa_result (*stop)(oa_driver *self);
  oa_result (*get_latency)(oa_driver *self, uint32_t *in_latency, uint32_t *out_latency);

  // Optional: set/get buffer size or sample rate while stopped
  oa_result (*set_sample_rate)(oa_driver *self, uint32_t sr);
  oa_result (*set_buffer_frames)(oa_driver *self, uint32_t frames);
} oa_driver_vtable;

// Driver opaque instance
struct oa_driver {
  const oa_driver_vtable *vt;
};

// Factory symbol every driver must export (C ABI, unmangled):
//   int32_t openasio_driver_create(const oa_create_params*, oa_driver** out);
//   void    openasio_driver_destroy(oa_driver*);
typedef int32_t (*openasio_driver_create_fn)(const oa_create_params*, oa_driver**);
typedef void    (*openasio_driver_destroy_fn)(oa_driver*);

#ifdef __cplusplus
}
#endif
#endif // OPENASIO_H
