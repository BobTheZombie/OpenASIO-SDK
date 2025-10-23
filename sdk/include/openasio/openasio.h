/*
 OpenASIO: permissive, ASIO-like realtime audio driver ABI.
 Version: 0.2.0 (draft) — NOT affiliated with Steinberg ASIO®.
 License: MIT OR Apache-2.0
*/
#ifndef OPENASIO_H
#define OPENASIO_H
#ifdef __cplusplus
extern "C" { 
#endif

#include <stdint.h>
#include <stddef.h>

#define OA_VERSION_MAJOR 0
#define OA_VERSION_MINOR 2
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
  OA_OK             =  0,
  OA_ERR_GENERIC    = -1,
  OA_ERR_UNSUPPORTED= -2,
  OA_ERR_INVALID_ARG= -3,
  OA_ERR_DEVICE     = -4,
  OA_ERR_BACKEND    = -5,
  OA_ERR_STATE      = -6,
} oa_result;

typedef enum {
  OA_SAMPLE_F32 = 1,   // native float32 [-1,+1]
  OA_SAMPLE_I16 = 2,
  OA_SAMPLE_U16 = 3,
} oa_sample_format;

typedef enum {
  OA_BUF_INTERLEAVED   = 1, // frames*channels
  OA_BUF_NONINTERLEAVED= 2, // array of channel pointers
} oa_buffer_layout;

// Capability bitfield (bitwise OR)
typedef enum {
  OA_CAP_OUTPUT        = 1<<0,
  OA_CAP_INPUT         = 1<<1,
  OA_CAP_FULL_DUPLEX   = 1<<2,
  OA_CAP_SET_SAMPLERATE= 1<<3,
  OA_CAP_SET_BUFFRAMES = 1<<4,
} oa_caps;

typedef struct {
  uint32_t sample_rate;     // Hz
  uint32_t buffer_frames;   // frames per callback (target; driver may adjust)
  uint16_t in_channels;     // inputs
  uint16_t out_channels;    // outputs
  oa_sample_format format;  // sample format
  oa_buffer_layout layout;  // interleaved/non-interleaved
} oa_stream_config;

typedef struct {
  uint64_t host_time_ns;    // host monotonic time
  uint64_t device_time_ns;  // device clock (0 if unknown)
  uint32_t underruns;       // since last callback
  uint32_t overruns;        // since last callback
} oa_time_info;

struct oa_driver;
typedef struct oa_driver oa_driver;

// Host callbacks: invoked by the driver on its RT thread.
typedef struct {
  // In non-interleaved mode: `in` is const void** (one per input ch), `out` is void** (one per output ch).
  // In interleaved mode:     `in` is const void* samples,          `out` is void* samples.
  oa_bool (*process)(void *user,
                     const void *in,
                     void *out,
                     uint32_t frames,
                     const oa_time_info *time,
                     const oa_stream_config *cfg);
  void (*latency_changed)(void *user, uint32_t input_latency, uint32_t output_latency); // optional
  void (*reset_request)(void *user); // optional
} oa_host_callbacks;

// Creation parameters for a driver instance
typedef struct {
  uint32_t struct_size;      // set to sizeof(oa_create_params)
  const oa_host_callbacks *host;
  void *host_user;
} oa_create_params;

// Function table implemented by the driver
typedef struct {
  uint32_t struct_size; // sizeof(oa_driver_vtable)

  // Capabilities bit mask (OR of oa_caps)
  uint32_t (*get_caps)(oa_driver *self);

  // Optional device enumeration: newline-separated names into buf. Returns OA_OK or error.
  oa_result (*query_devices)(oa_driver *self, char *buf, size_t buf_len);

  // Open by name (NULL or "" = default). Returns >=0 device_id or <0 error.
  int32_t  (*open_device)(oa_driver *self, const char *name);
  oa_result (*close_device)(oa_driver *self);

  // Retrieve a default config for the currently open device.
  oa_result (*get_default_config)(oa_driver *self, oa_stream_config *out);

  // Start/stop streaming. On start, driver begins invoking host.process() on its RT thread.
  oa_result (*start)(oa_driver *self, const oa_stream_config *cfg);
  oa_result (*stop)(oa_driver *self);

  // Latency reporting in frames (<=0 if unknown).
  oa_result (*get_latency)(oa_driver *self, uint32_t *in_latency, uint32_t *out_latency);

  // Optional reconfiguration while stopped.
  oa_result (*set_sample_rate)(oa_driver *self, uint32_t sr);
  oa_result (*set_buffer_frames)(oa_driver *self, uint32_t frames);
} oa_driver_vtable;

// Opaque driver instance
struct oa_driver {
  const oa_driver_vtable *vt;
};

// Mandatory factory symbols (C ABI)
typedef int32_t (*openasio_driver_create_fn)(const oa_create_params*, oa_driver**);
typedef void    (*openasio_driver_destroy_fn)(oa_driver*);

#ifdef __cplusplus
}
#endif
#endif // OPENASIO_H
